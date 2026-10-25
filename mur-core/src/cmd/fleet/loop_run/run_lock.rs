//! Per-fleet run lock: a fleet run holds an exclusive OS advisory lock on
//! `run.lock` in its state directory for as long as it runs.
//!
//! Liveness is decided ONLY by whether the lock can be taken. The kernel
//! drops the lock when the holder exits for any reason, SIGKILL included, so
//! a dead run never looks alive and a recycled pid cannot fake liveness. The
//! pid in `run.owner` never decides liveness: it is shown to the operator, and
//! [`is_held_by`] uses it only to tell WHICH live holder took the lock. Same
//! approach as the review driver's lock (`review/run_lock.rs`), over a different file.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::Utc;
use fs2::FileExt;
use mur_common::fleet::valid_fleet_name;
use mur_common::paths::{FLEET_RUN_LOCK_FILE, FLEET_RUN_OWNER_FILE, fleet_state_dir};

/// A held run lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct RunLock(#[allow(dead_code)] File);

/// Take the run lock for `name` without blocking, or say who holds it.
pub fn acquire(mur_home: &Path, name: &str) -> Result<RunLock> {
    // Validate before touching the filesystem: the name is joined into a
    // path, so `..` or an absolute path would create files outside
    // `fleet-state/`.
    if !valid_fleet_name(name) {
        bail!("invalid fleet name '{name}'");
    }
    let dir = fleet_state_dir(mur_home, name);
    std::fs::create_dir_all(&dir)?;
    acquire_at(&dir.join(FLEET_RUN_LOCK_FILE), name)
}

fn acquire_at(path: &Path, name: &str) -> Result<RunLock> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
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
            Ok(RunLock(f))
        }
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
            match describe(path) {
                Some(who) => bail!("a run of '{name}' is live ({who})"),
                None => bail!("a run of '{name}' is live"),
            }
        }
        Err(e) => Err(anyhow::Error::new(e)
            .context(format!("the run lock of fleet '{name}' cannot be taken"))),
    }
}

/// Is a run of `name` holding the lock right now? Probes with a try-lock and
/// releases it at once, so it never blocks and never leaves the lock held.
/// `false` when the lock file does not exist (no run has ever taken it).
pub fn is_held(mur_home: &Path, name: &str) -> bool {
    if !valid_fleet_name(name) {
        return false;
    }
    let path = fleet_state_dir(mur_home, name).join(FLEET_RUN_LOCK_FILE);
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
    else {
        return false;
    };
    match f.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&f);
            false
        }
        Err(e) => e.raw_os_error() == fs2::lock_contended_error().raw_os_error(),
    }
}

/// Is the lock held right now, by the process `pid`? Liveness still comes from
/// the try-lock alone; the owner file only says who took it. `false` while the
/// holder has not written its owner file yet, so a caller polls.
///
/// `mur fleet resume --detach` needs this, not [`is_held`]: when two resumes
/// race, the loser's child must not read the winner's lock as its own start.
pub fn is_held_by(mur_home: &Path, name: &str, pid: u32) -> bool {
    if !is_held(mur_home, name) {
        return false;
    }
    let lock = fleet_state_dir(mur_home, name).join(FLEET_RUN_LOCK_FILE);
    owner_pid(&lock) == Some(pid)
}

fn owner_pid(lock: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(owner_path(lock)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("pid")?.as_u64().and_then(|p| u32::try_from(p).ok())
}

fn owner_path(lock: &Path) -> PathBuf {
    lock.with_file_name(FLEET_RUN_OWNER_FILE)
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

    #[test]
    fn second_run_of_the_same_fleet_is_refused_then_allowed_after_release() {
        let home = tempfile::tempdir().unwrap();
        let first = acquire(home.path(), "f").expect("first run takes the lock");
        let err = acquire(home.path(), "f").expect_err("second run must be refused");
        let msg = format!("{err:#}");
        assert!(msg.contains("a run of 'f' is live"), "{msg}");
        assert!(
            msg.contains(&format!("pid {}", std::process::id())),
            "{msg}"
        );
        drop(first);
        acquire(home.path(), "f").expect("lock is free after the holder drops it");
    }

    #[test]
    fn different_fleets_do_not_contend() {
        let home = tempfile::tempdir().unwrap();
        let _a = acquire(home.path(), "a").unwrap();
        let _b = acquire(home.path(), "b").expect("another fleet is unaffected");
    }

    #[test]
    fn a_stored_pid_never_makes_a_free_lock_look_held() {
        let home = tempfile::tempdir().unwrap();
        let dir = fleet_state_dir(home.path(), "f");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(FLEET_RUN_OWNER_FILE),
            format!(r#"{{"pid":{}}}"#, std::process::id()),
        )
        .unwrap();
        acquire(home.path(), "f").expect("a live pid in the body must not block");
    }

    #[test]
    fn is_held_by_names_the_holder_only() {
        let home = tempfile::tempdir().unwrap();
        let me = std::process::id();
        assert!(!is_held_by(home.path(), "f", me), "no lock file yet");
        let lock = acquire(home.path(), "f").unwrap();
        assert!(is_held_by(home.path(), "f", me));
        assert!(!is_held_by(home.path(), "f", me.wrapping_add(1)));
        drop(lock);
        assert!(
            !is_held_by(home.path(), "f", me),
            "a stale owner file is not a holder"
        );
    }

    #[test]
    fn an_invalid_name_is_refused_before_any_filesystem_write() {
        let home = tempfile::tempdir().unwrap();
        for bad in ["../escape", "/abs/path", "a/b", "", ".."] {
            let err = acquire(home.path(), bad).expect_err(bad);
            assert!(format!("{err:#}").contains("invalid fleet name"), "{bad}");
        }
        let entries = std::fs::read_dir(home.path()).unwrap().count();
        assert_eq!(entries, 0, "no directory or file may be created");
    }
}
