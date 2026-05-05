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
