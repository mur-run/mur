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
