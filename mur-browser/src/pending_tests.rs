use super::*;
use crate::state::write_state;
use std::time::Duration;

#[derive(Default)]
struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, String>>);
impl StateKeyStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
}

const STATE: &[u8] = br#"{"cookies":[],"origins":[]}"#;

fn far() -> Instant {
    Instant::now() + abandon::LOCK_WAIT
}

fn setup(home: &Path, store: &MemoryStore) {
    std::fs::create_dir_all(paths::profile_dir(home, "a")).unwrap();
    write_state(&paths::profile_state(home, "a"), STATE, store).unwrap();
}

fn markers(home: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(paths::profile_dir(home, "a"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(MARKER_PREFIX))
        })
        .collect()
}

fn sweep_now(home: &Path) -> Option<Kept> {
    let held = abandon::lock(home, "a", far()).unwrap().unwrap();
    sweep(home, "a", &held).unwrap()
}

#[test]
fn a_live_marker_is_never_swept_and_a_dropped_one_is_gone() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (loaded, plain) = load(home.path(), "a", &store, far()).unwrap();
    assert_eq!(plain, STATE);
    assert_eq!(markers(home.path()).len(), 1);
    assert_eq!(sweep_now(home.path()), None, "its owner still holds it");
    assert_eq!(markers(home.path()).len(), 1);
    drop(loaded);
    assert!(
        markers(home.path()).is_empty(),
        "a resolved session leaves nothing"
    );
}

#[test]
fn two_sessions_on_one_profile_get_two_markers() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (one, _) = load(home.path(), "a", &store, far()).unwrap();
    let (two, _) = load(home.path(), "a", &store, far()).unwrap();
    assert_eq!(markers(home.path()).len(), 2);
    assert_eq!(one.version, two.version, "same file, same watermark");
    drop(one);
    assert_eq!(markers(home.path()).len(), 1, "only its own marker goes");
    drop(two);
}

#[test]
fn a_kept_marker_is_reported_once_with_its_reason() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (loaded, _) = load(home.path(), "a", &store, far()).unwrap();
    loaded.marker.keep(Kept::LockTimeout);
    assert_eq!(sweep_now(home.path()), Some(Kept::LockTimeout));
    assert!(markers(home.path()).is_empty());
    assert_eq!(sweep_now(home.path()), None, "reported once");

    let (loaded, _) = load(home.path(), "a", &store, far()).unwrap();
    loaded.marker.keep(Kept::Interrupted);
    assert_eq!(sweep_now(home.path()), Some(Kept::Interrupted));
}

#[test]
fn a_marker_older_than_the_last_write_is_removed_silently() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let (loaded, _) = load(home.path(), "a", &store, far()).unwrap();
    loaded.marker.keep(Kept::Interrupted);
    // A later login rewrote the profile after the session died.
    std::thread::sleep(Duration::from_millis(20));
    write_state(&paths::profile_state(home.path(), "a"), STATE, &store).unwrap();
    assert_eq!(sweep_now(home.path()), None);
    assert!(markers(home.path()).is_empty());
}

/// Set on the re-exec'd lock holder: the marker it must lock, and the file it
/// creates once it holds the lock.
const HOLD_MARKER_ENV: &str = "MUR_TEST_HOLD_MARKER";
const HOLD_READY_ENV: &str = "MUR_TEST_HOLD_READY";
/// How long the parent waits for the holder to lock, and for the OS to
/// release the lock after the kill (Windows releases it asynchronously).
const HOLD_WAIT: Duration = Duration::from_secs(20);

/// Not a test of its own: the lock-holder body the killed-session test
/// re-execs this binary into. Without the env var it does nothing.
#[test]
#[ignore = "re-exec'd as a child by a_killed_session_leaves_a_marker_the_next_use_reports"]
fn lock_holder_child() {
    let (Ok(marker), Ok(ready)) = (
        std::env::var(HOLD_MARKER_ENV),
        std::env::var(HOLD_READY_ENV),
    ) else {
        return;
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&marker)
        .unwrap();
    file.try_lock_exclusive().unwrap();
    std::fs::write(&ready, b"").unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

/// Kills the holder even when an assertion panics first, so a failing run
/// does not leave it sleeping on the marker.
struct Holder(std::process::Child);
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn poll_until<T>(mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + HOLD_WAIT;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The spec's case: the proxy is killed mid write-back. `Drop` never runs,
/// the OS releases the marker's lock with the process, and the next use
/// reports it. The holder is this test binary re-exec'd, locking with the
/// same `fs2` call as [`Marker::create`], so it runs on every platform.
#[test]
fn a_killed_session_leaves_a_marker_the_next_use_reports() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let dir = paths::profile_dir(home.path(), "a");
    let marker = dir.join(format!("{MARKER_PREFIX}999999-1"));
    std::fs::write(&marker, b"").unwrap();
    let ready = home.path().join("holder.ready");
    let mut holder = Holder(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pending::tests::lock_holder_child",
                "--include-ignored",
                "--nocapture",
            ])
            .env(HOLD_MARKER_ENV, &marker)
            .env(HOLD_READY_ENV, &ready)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let locked = poll_until(|| {
        if let Ok(Some(status)) = holder.0.try_wait() {
            panic!("lock holder exited early: {status}");
        }
        ready.exists().then_some(())
    });
    assert!(locked.is_some(), "lock holder never took the lock");
    assert_eq!(sweep_now(home.path()), None, "alive while its owner runs");
    assert!(marker.exists(), "a live marker is left in place");
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let kept = poll_until(|| sweep_now(home.path()));
    assert_eq!(kept, Some(Kept::Interrupted));
    assert!(!marker.exists(), "a dead marker is removed once reported");
    let n = notice("a", Kept::Interrupted);
    assert!(n.contains("may not have been saved"), "{n}");
}
