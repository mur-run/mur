//! Integration tests for `bridge::beacon::bridge_status_for_peer`.
//!
//! Plan task M-c1.4.2 — verifies the Running / Degraded / Offline
//! classification by manipulating `running.lock` mtime in a tempdir.

use mur_agent_runtime::bridge::beacon::{BridgePeerStatus, bridge_status_for_peer};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn write_lock_with_age(dir: &std::path::Path, age: Duration) {
    let lock = dir.join("running.lock");
    std::fs::write(&lock, b"{}").unwrap();
    // Windows requires write access to set mtime; std::fs::File::open is read-only.
    let f = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

#[test]
fn fresh_lock_is_running() {
    let tmp = TempDir::new().unwrap();
    write_lock_with_age(tmp.path(), Duration::from_secs(5));
    assert_eq!(
        bridge_status_for_peer(tmp.path()),
        BridgePeerStatus::Running
    );
}

#[test]
fn old_lock_is_degraded() {
    let tmp = TempDir::new().unwrap();
    write_lock_with_age(tmp.path(), Duration::from_secs(120));
    assert_eq!(
        bridge_status_for_peer(tmp.path()),
        BridgePeerStatus::Degraded
    );
}

#[test]
fn missing_lock_is_offline() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        bridge_status_for_peer(tmp.path()),
        BridgePeerStatus::Offline
    );
}
