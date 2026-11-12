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

/// The spec's case: the proxy is killed mid write-back. `Drop` never runs,
/// the OS releases the marker's lock with the process, and the next use
/// reports it.
#[cfg(unix)]
#[test]
fn a_killed_session_leaves_a_marker_the_next_use_reports() {
    let home = tempfile::tempdir().unwrap();
    let store = MemoryStore::default();
    setup(home.path(), &store);
    let dir = paths::profile_dir(home.path(), "a");
    let marker = dir.join(format!("{MARKER_PREFIX}999999-1"));
    std::fs::write(&marker, b"").unwrap();
    let code = format!(
        "import fcntl,time; f=open({p:?},'a'); fcntl.flock(f, fcntl.LOCK_EX); print(1, flush=True); time.sleep(60)",
        p = marker.display().to_string()
    );
    let mut child = std::process::Command::new("python3")
        .args(["-c", &code])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = [0u8; 1];
    std::io::Read::read_exact(child.stdout.as_mut().unwrap(), &mut ready).unwrap();
    assert_eq!(sweep_now(home.path()), None, "alive while its owner runs");
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(sweep_now(home.path()), Some(Kept::Interrupted));
    let n = notice("a", Kept::Interrupted);
    assert!(n.contains("may not have been saved"), "{n}");
}
