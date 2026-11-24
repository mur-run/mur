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
use mur_common::paths::{
    FLEET_RUN_LOCK_FILE, FLEET_RUN_ONE_SHOT_LOCK_FILE, FLEET_RUN_OWNER_FILE, fleet_state_dir,
};

/// A held run lock. Dropping it releases the lock.
///
/// Fields drop in declaration order: the run lock is released first and the
/// one-shot marker after it, so whenever the run lock is held by a one-shot,
/// its marker is held too.
#[derive(Debug)]
pub struct RunLock {
    #[allow(dead_code)]
    run: File,
    #[allow(dead_code)]
    one_shot: Option<File>,
}

/// What kind of run takes the lock. A one-shot reads no pause marker, so
/// `pause` must be able to tell it from a pausable `--loop` run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HolderKind {
    Loop,
    OneShot,
}

/// Take the run lock for a `--loop` run of `name` without blocking, or say
/// who holds it.
pub fn acquire(mur_home: &Path, name: &str) -> Result<RunLock> {
    acquire_as(mur_home, name, HolderKind::Loop)
}

/// [`acquire`] for a one-shot run: same lock, plus the one-shot marker that
/// makes it read as not pausable (#2554).
pub fn acquire_one_shot(mur_home: &Path, name: &str) -> Result<RunLock> {
    acquire_as(mur_home, name, HolderKind::OneShot)
}

fn acquire_as(mur_home: &Path, name: &str, kind: HolderKind) -> Result<RunLock> {
    // Validate before touching the filesystem: the name is joined into a
    // path, so `..` or an absolute path would create files outside
    // `fleet-state/`.
    if !valid_fleet_name(name) {
        bail!("invalid fleet name '{name}'");
    }
    let dir = fleet_state_dir(mur_home, name);
    std::fs::create_dir_all(&dir)?;
    let lock = take_locks(&dir.join(FLEET_RUN_LOCK_FILE), name, kind)?;
    write_owner(&dir.join(FLEET_RUN_LOCK_FILE));
    Ok(lock)
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// Take the locks only; [`write_owner`] describes the holder afterwards. Kept
/// apart so a test can stand in the window between the two.
fn take_locks(path: &Path, name: &str, kind: HolderKind) -> Result<RunLock> {
    // The marker comes FIRST: once the run lock is observably held by a
    // one-shot, the marker already is. Shared, so two one-shots racing for
    // the run lock both get this far; blocking, so a `pause` probe holding it
    // exclusively for an instant delays the run instead of failing it.
    let one_shot = match kind {
        HolderKind::Loop => None,
        HolderKind::OneShot => {
            let f = open_lock_file(&one_shot_path(path))?;
            f.lock_shared().map_err(|e| {
                anyhow::Error::new(e).context(format!(
                    "the one-shot marker of fleet '{name}' cannot be taken"
                ))
            })?;
            Some(f)
        }
    };
    let f = open_lock_file(path)?;
    match f.try_lock_exclusive() {
        Ok(()) => Ok(RunLock { run: f, one_shot }),
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

/// A one-shot that has taken its locks but not yet written its owner file: the
/// window a concurrent `pause` can land in. Tests only.
#[cfg(test)]
pub(crate) fn acquire_one_shot_before_owner_write(mur_home: &Path, name: &str) -> RunLock {
    let dir = fleet_state_dir(mur_home, name);
    std::fs::create_dir_all(&dir).unwrap();
    take_locks(&dir.join(FLEET_RUN_LOCK_FILE), name, HolderKind::OneShot).unwrap()
}

/// Best effort: the owner file is display-only plus [`is_held_by`]'s pid, so a
/// body we cannot write only costs a message. Nothing reads a run's kind here.
fn write_owner(lock: &Path) {
    let host = hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_default();
    let body = serde_json::json!({
        "pid": std::process::id(),
        "started_at": Utc::now(),
        "host": host,
    });
    let _ = std::fs::write(owner_path(lock), body.to_string());
}

/// Is a run of `name` holding the lock right now? Probes with a try-lock and
/// releases it at once, so it never blocks and never leaves the lock held.
/// `false` when the lock file does not exist (no run has ever taken it).
pub fn is_held(mur_home: &Path, name: &str) -> bool {
    if !valid_fleet_name(name) {
        return false;
    }
    probe_held(&fleet_state_dir(mur_home, name).join(FLEET_RUN_LOCK_FILE))
}

/// Is some process holding a lock on `path` right now? Try-locks and releases
/// at once; a missing file is not held.
fn probe_held(path: &Path) -> bool {
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
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

/// Is the lock held right now by a one-shot run? Decided by the kernel's lock
/// state alone, never by the owner file: a one-shot holds its marker from
/// before it takes the run lock until after it releases it, so there is no
/// moment where a one-shot holds the run lock unmarked, and a crashed run's
/// stale owner file cannot speak for the live holder (#2554).
///
/// A one-shot still contending for the run lock holds the marker briefly too,
/// so a live loop can read as a one-shot for that instant. That errs toward
/// refusing a pause, which the operator retries, never toward waiting out
/// `PAUSE_WAIT` on a run that will not read the marker.
pub fn is_held_by_one_shot(mur_home: &Path, name: &str) -> bool {
    if !is_held(mur_home, name) {
        return false;
    }
    let lock = fleet_state_dir(mur_home, name).join(FLEET_RUN_LOCK_FILE);
    probe_held(&one_shot_path(&lock))
}

fn one_shot_path(lock: &Path) -> PathBuf {
    lock.with_file_name(FLEET_RUN_ONE_SHOT_LOCK_FILE)
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

    /// #2554: only a live one-shot holder reads as one, and the owner file has
    /// no say: a loop's stale record cannot hide a one-shot, and a one-shot's
    /// stale record cannot make a later loop read as one.
    #[test]
    fn is_held_by_one_shot_follows_the_live_holder() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert!(!is_held_by_one_shot(h, "f"), "no lock file yet");
        let one_shot = acquire_one_shot(h, "f").unwrap();
        assert!(is_held_by_one_shot(h, "f"));
        drop(one_shot);
        assert!(
            !is_held_by_one_shot(h, "f"),
            "a released marker is not a holder"
        );
        let looped = acquire(h, "f").unwrap();
        assert!(!is_held_by_one_shot(h, "f"), "a loop holds no marker");
        drop(looped);
    }

    /// #2554 review: the run lock is observable before the owner file is
    /// rewritten. A one-shot caught in that window, over a crashed loop's
    /// owner file, must already read as a one-shot.
    #[test]
    fn a_one_shot_reads_as_one_before_its_owner_file_is_written() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        drop(acquire(h, "f").unwrap());
        let owner = fleet_state_dir(h, "f").join(FLEET_RUN_OWNER_FILE);
        let stale = std::fs::read_to_string(&owner).unwrap();
        let held = acquire_one_shot_before_owner_write(h, "f");
        assert_eq!(std::fs::read_to_string(&owner).unwrap(), stale, "window");
        assert!(is_held(h, "f"));
        assert!(is_held_by_one_shot(h, "f"));
        // A torn owner file changes nothing either.
        std::fs::write(&owner, "{\"pid\":").unwrap();
        assert!(is_held_by_one_shot(h, "f"));
        drop(held);
        assert!(!is_held(h, "f"));
        assert!(!is_held_by_one_shot(h, "f"));
    }

    /// A one-shot refused for contention gives its marker back, so the live
    /// loop it lost to reads as a loop again.
    #[test]
    fn a_refused_one_shot_releases_its_marker() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let looped = acquire(h, "f").unwrap();
        acquire_one_shot(h, "f").expect_err("the loop holds the lock");
        assert!(is_held(h, "f"));
        assert!(!is_held_by_one_shot(h, "f"));
        drop(looped);
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
