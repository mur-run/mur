use super::*;
use crate::entitlements_pin::lock_profile_until;
use std::time::{Duration, Instant};

fn lock_for(mur_home: &Path, agent: &str) -> ProfileLock {
    lock_profile_until(mur_home, agent, Instant::now() + Duration::from_secs(2)).unwrap()
}

fn home(mur_home: &Path, agent: &str) -> PathBuf {
    let dir = mur_home.join("agents").join(agent);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn entries(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn a_lock_for_a_is_refused_by_the_writer_for_b() {
    let m = tempfile::tempdir().unwrap();
    let a_home = home(m.path(), "a");
    let b_home = home(m.path(), "b");
    std::fs::write(b_home.join(PROFILE_FILE), b"old").unwrap();
    let lock_a = lock_for(m.path(), "a");
    let err = write_installed_profile(&lock_a, "b", ProfileExpect::Exists, b"new").unwrap_err();
    assert!(
        matches!(err, ProfileWriteError::WrongLock { .. }),
        "{err:?}"
    );
    assert_eq!(std::fs::read(b_home.join(PROFILE_FILE)).unwrap(), b"old");
    assert_eq!(entries(&a_home), Vec::<String>::new());
    assert_eq!(entries(&b_home), [PROFILE_FILE]);
}

#[test]
fn exists_overwrites_a_present_profile_and_leaves_no_temp_file() {
    let m = tempfile::tempdir().unwrap();
    let dir = home(m.path(), "a");
    std::fs::write(dir.join(PROFILE_FILE), b"old").unwrap();
    let lock = lock_for(m.path(), "a");
    let out = write_installed_profile(&lock, "a", ProfileExpect::Exists, b"new").unwrap();
    assert_eq!(out, WriteOutcome::Written);
    assert_eq!(std::fs::read(dir.join(PROFILE_FILE)).unwrap(), b"new");
    assert_eq!(entries(&dir), [PROFILE_FILE]);
}

#[test]
fn exists_skips_a_missing_profile() {
    let m = tempfile::tempdir().unwrap();
    let dir = home(m.path(), "a");
    let lock = lock_for(m.path(), "a");
    let out = write_installed_profile(&lock, "a", ProfileExpect::Exists, b"new").unwrap();
    assert_eq!(out, WriteOutcome::Skipped);
    assert_eq!(entries(&dir), Vec::<String>::new(), "nothing created");
}

#[test]
fn absent_refuses_an_existing_profile_and_writes_a_missing_one() {
    let m = tempfile::tempdir().unwrap();
    let dir = home(m.path(), "a");
    let lock = lock_for(m.path(), "a");
    let out = write_installed_profile(&lock, "a", ProfileExpect::Absent, b"first").unwrap();
    assert_eq!(out, WriteOutcome::Written);
    let err = write_installed_profile(&lock, "a", ProfileExpect::Absent, b"second").unwrap_err();
    assert!(
        matches!(err, ProfileWriteError::AlreadyExists(_)),
        "{err:?}"
    );
    assert_eq!(std::fs::read(dir.join(PROFILE_FILE)).unwrap(), b"first");
    assert_eq!(entries(&dir), [PROFILE_FILE]);
}

#[test]
fn two_concurrent_writers_never_see_each_others_temp_file() {
    let m = tempfile::tempdir().unwrap();
    let dir = m.path().join("embedded").join("x");
    std::fs::create_dir_all(&dir).unwrap();
    let payload = |tag: u8| vec![tag; 64 * 1024];
    let threads: Vec<_> = (*b"AB")
        .into_iter()
        .map(|tag| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    write_other_profile(&dir, &payload(tag)).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let body = std::fs::read(dir.join(PROFILE_FILE)).unwrap();
    assert!(body == payload(b'A') || body == payload(b'B'), "torn write");
    assert_eq!(entries(&dir), [PROFILE_FILE], "a temp file was left");
}

#[test]
fn write_other_profile_refuses_an_installed_path_and_writes_elsewhere() {
    let m = tempfile::tempdir().unwrap();
    let installed = home(m.path(), "a");
    let err = write_other_profile(&installed, b"x").unwrap_err();
    assert!(
        matches!(err, ProfileWriteError::InstalledHome(_)),
        "{err:?}"
    );
    assert_eq!(entries(&installed), Vec::<String>::new());

    let other = m.path().join("cache").join("a");
    std::fs::create_dir_all(&other).unwrap();
    write_other_profile(&other, b"x").unwrap();
    assert_eq!(std::fs::read(other.join(PROFILE_FILE)).unwrap(), b"x");
}
