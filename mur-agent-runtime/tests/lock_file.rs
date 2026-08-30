use mur_agent_runtime::lock_file::{LockError, LockHandle, is_stale, read_lock, write_lock};
use mur_common::LockFile;
use std::fs;
use tempfile::TempDir;

fn sample_lock() -> LockFile {
    LockFile {
        schema: 1,
        uuid: "01JQX4TM8Y9K7VQH6B2N3R5DPE".into(),
        name: "agent_a".into(),
        pid: std::process::id(),
        ppid: 1,
        started_at: "2026-04-22T08:00:00Z".into(),
        binary_version: "mur-agent-runtime 0.1.0".into(),
        transports: mur_common::agent::LockTransports {
            stdio: false,
            unix_socket: Some("/tmp/x.sock".into()),
            tcp: None,
            webhook: None,
        },
        card_digest: "sha256:abc".into(),
        capabilities: vec!["a2a.message.send".into()],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
    }
}

#[test]
fn write_and_read_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let lock = sample_lock();
    let _handle = LockHandle::acquire(&path).unwrap();
    write_lock(&path, &lock).unwrap();
    let got = read_lock(&path).unwrap();
    assert_eq!(got.uuid, lock.uuid);
    assert_eq!(got.pid, lock.pid);
}

#[test]
fn second_acquire_while_held_fails() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let _h1 = LockHandle::acquire(&path).unwrap();
    match LockHandle::acquire(&path) {
        Err(LockError::AlreadyHeld) => {}
        other => panic!("expected AlreadyHeld, got {other:?}"),
    }
}

#[test]
fn detects_stale_when_pid_dead() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let mut lock = sample_lock();
    lock.pid = 999_999; // almost certainly dead
    fs::write(&path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
    assert!(is_stale(&path).unwrap(), "dead pid should be stale");
}

#[test]
fn live_lock_not_stale() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let _handle = LockHandle::acquire(&path).unwrap();
    write_lock(&path, &sample_lock()).unwrap();
    assert!(!is_stale(&path).unwrap(), "held lock should not be stale");
}

#[test]
fn stale_sentinel_content_does_not_block_acquire() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let sentinel = tmp.path().join("running.sentinel");
    // Bogus pid text with no flock holder: acquire must still succeed since
    // ownership is determined by flock, not by file contents.
    fs::write(&sentinel, b"999999").unwrap();
    let _handle = LockHandle::acquire(&path).unwrap();
}

// Windows LockFileEx is a mandatory lock, so reading the sentinel while the
// flock is held from the same process is denied there (os error 33); unix
// flock is advisory, so the read-back succeeds.
#[cfg(unix)]
#[test]
fn sentinel_records_current_pid_after_acquire() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("running.lock");
    let sentinel = tmp.path().join("running.sentinel");
    let _handle = LockHandle::acquire(&path).unwrap();
    let contents = fs::read_to_string(&sentinel).unwrap();
    let pid: u32 = contents.trim().parse().unwrap();
    assert_eq!(pid, std::process::id());
}
