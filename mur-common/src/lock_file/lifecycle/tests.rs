use super::*;
use crate::lock_file::{SENTINEL_FILE, SentinelOpenError, agent_exists, open_sentinel};
use std::time::{Duration, Instant};

const SHORT: Duration = Duration::from_millis(150);
/// Scheduling slack on top of "deadline plus one poll".
const SLACK: Duration = Duration::from_millis(300);

fn lock_path(home: &Path) -> PathBuf {
    home.join(FLEET_SHARED_DIR).join(LIFECYCLE_LOCK_FILE)
}

/// Swap the lock file for a new one, the way a hand edit would.
fn replace(path: &Path) {
    let next = path.with_extension("next");
    std::fs::write(&next, b"").unwrap();
    std::fs::rename(&next, path).unwrap();
}

#[test]
fn creates_the_store_root_and_the_lock_file() {
    let home = tempfile::tempdir().unwrap();
    let _guard = lock_reply_dirs(home.path(), SHORT).unwrap();
    assert!(lock_path(home.path()).is_file());
}

#[test]
fn a_second_taker_gets_the_lock_once_the_first_guard_drops() {
    let home = tempfile::tempdir().unwrap();
    let first = lock_reply_dirs(home.path(), SHORT).unwrap();
    drop(first);
    lock_reply_dirs(home.path(), SHORT).expect("free after drop");
}

#[test]
fn a_second_taker_times_out_while_the_first_is_held() {
    let home = tempfile::tempdir().unwrap();
    let _first = lock_reply_dirs(home.path(), SHORT).unwrap();
    let started = Instant::now();
    let err = lock_reply_dirs(home.path(), SHORT).unwrap_err();
    let waited = started.elapsed();
    let LifecycleLockError::Timeout { path, .. } = &err else {
        panic!("expected a timeout, got {err:?}");
    };
    assert_eq!(path, &lock_path(home.path()));
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "{err}"
    );
    assert!(waited >= SHORT, "returned early: {waited:?}");
    assert!(waited < SHORT + LOCK_POLL + SLACK, "too late: {waited:?}");
}

#[test]
fn a_lock_file_replaced_once_ends_with_the_lock_on_the_new_file() {
    let home = tempfile::tempdir().unwrap();
    let path = lock_path(home.path());
    let mut replaced = false;
    let guard = lock_until(home.path(), Instant::now() + SHORT * 4, |p| {
        if !replaced {
            replaced = true;
            replace(p);
        }
    })
    .expect("second attempt meets the new file");
    assert!(replaced);
    assert!(guard.still_at_path().unwrap(), "guard is on the old file");
    // The new file is the held one: nobody else can take it.
    let other = lock_reply_dirs(home.path(), Duration::from_millis(50));
    assert!(matches!(other, Err(LifecycleLockError::Timeout { .. })));
    assert!(path.is_file());
}

#[test]
fn a_lock_file_replaced_on_every_attempt_ends_in_the_timeout_error() {
    let home = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let mut attempts = 0u32;
    let err = lock_until(home.path(), Instant::now() + SHORT, |p| {
        attempts += 1;
        replace(p);
    })
    .unwrap_err();
    let waited = started.elapsed();
    assert!(matches!(err, LifecycleLockError::Timeout { .. }), "{err:?}");
    assert!(attempts > 1, "never retried: {attempts}");
    assert!(waited >= SHORT, "returned early: {waited:?}");
    assert!(waited < SHORT + LOCK_POLL + SLACK, "too late: {waited:?}");
}

#[test]
fn mur_home_is_synced_after_the_store_root_is_created() {
    let home = tempfile::tempdir().unwrap();
    take_synced_dirs();
    drop(lock_reply_dirs(home.path(), SHORT).unwrap());
    assert_eq!(take_synced_dirs(), vec![home.path().to_path_buf()]);
    // The root exists now, so a second take creates nothing to make durable.
    drop(lock_reply_dirs(home.path(), SHORT).unwrap());
    assert!(take_synced_dirs().is_empty());
}

#[test]
fn agent_exists_is_false_for_a_home_that_holds_only_a_sentinel() {
    let home = tempfile::tempdir().unwrap();
    let agent = home.path().join("agents").join("a");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join(SENTINEL_FILE), b"").unwrap();
    assert!(!agent_exists(home.path(), "a"));
    std::fs::write(agent.join("profile.yaml"), b"name: a\n").unwrap();
    assert!(agent_exists(home.path(), "a"));
}

#[test]
fn agent_exists_is_false_for_a_bad_name_a_directory_or_nothing() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join("agents");
    std::fs::create_dir_all(agents.join("d").join("profile.yaml")).unwrap();
    assert!(!agent_exists(home.path(), "d"), "profile.yaml is a dir");
    assert!(!agent_exists(home.path(), "missing"));
    assert!(!agent_exists(home.path(), "../agents/d"));
    assert!(!agent_exists(home.path(), ""));
}

#[test]
fn open_sentinel_never_creates_the_home_and_tells_the_two_absences_apart() {
    let m = tempfile::tempdir().unwrap();
    let no_agents = m.path().join("agents").join("a");
    assert!(matches!(
        open_sentinel(&no_agents),
        Err(SentinelOpenError::NoAgentsDir(_))
    ));
    assert!(!m.path().join("agents").exists(), "agents/ was created");

    std::fs::create_dir_all(m.path().join("agents")).unwrap();
    assert!(matches!(
        open_sentinel(&no_agents),
        Err(SentinelOpenError::NoHome(_))
    ));
    assert!(!no_agents.exists(), "the home was created");
}

#[test]
fn open_sentinel_creates_the_file_in_an_existing_home_and_reuses_it() {
    let m = tempfile::tempdir().unwrap();
    let home = m.path().join("agents").join("a");
    std::fs::create_dir_all(&home).unwrap();
    let first = open_sentinel(&home).unwrap();
    let second = open_sentinel(&home).unwrap();
    assert!(same_file(&first, &home.join(SENTINEL_FILE)).unwrap());
    assert!(same_file(&second, &home.join(SENTINEL_FILE)).unwrap());
}

#[test]
fn probe_sentinel_reports_no_agent_dir_for_both_absences() {
    let m = tempfile::tempdir().unwrap();
    let home = m.path().join("agents").join("a");
    assert!(matches!(
        crate::lock_file::probe_sentinel(&home),
        crate::lock_file::SentinelProbe::NoAgentDir
    ));
    std::fs::create_dir_all(m.path().join("agents")).unwrap();
    assert!(matches!(
        crate::lock_file::probe_sentinel(&home),
        crate::lock_file::SentinelProbe::NoAgentDir
    ));
    assert!(!home.exists());
}
