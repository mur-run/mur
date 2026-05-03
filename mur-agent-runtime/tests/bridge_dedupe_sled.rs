use mur_agent_runtime::bridge::dedupe::DedupeStore;
use tempfile::TempDir;

#[test]
fn mark_then_is_seen_returns_true() {
    let tmp = TempDir::new().unwrap();
    let mut s = DedupeStore::open(tmp.path(), "bridge_telegram").unwrap();
    assert!(!s.is_seen("msg-42").unwrap());
    s.mark_seen("msg-42").unwrap();
    assert!(s.is_seen("msg-42").unwrap());
}

#[test]
fn unseen_returns_false() {
    let tmp = TempDir::new().unwrap();
    let s = DedupeStore::open(tmp.path(), "x").unwrap();
    assert!(!s.is_seen("never-marked").unwrap());
}

#[test]
fn different_bridges_independent() {
    let tmp = TempDir::new().unwrap();
    let mut a = DedupeStore::open(tmp.path(), "a").unwrap();
    a.mark_seen("msg-1").unwrap();
    drop(a);
    // separate bridge_id namespace inside the same DB
    let b_dir = tmp.path().join("b");
    std::fs::create_dir(&b_dir).unwrap();
    let b = DedupeStore::open(&b_dir, "b").unwrap();
    assert!(!b.is_seen("msg-1").unwrap());
}

#[test]
fn ttl_eviction_removes_old_entries() {
    let tmp = TempDir::new().unwrap();
    let mut s = DedupeStore::open(tmp.path(), "bridge").unwrap();
    let stale_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - (8 * 24 * 60 * 60);
    s.insert_at_for_test("stale", stale_ts).unwrap();
    s.mark_seen("fresh").unwrap();
    let evicted = s.sweep_expired().unwrap();
    assert_eq!(evicted, 1);
    assert!(!s.is_seen("stale").unwrap());
    assert!(s.is_seen("fresh").unwrap());
}
