//! Day-1 smoke test: prove the 6 ops happy path works end-to-end on a fresh
//! tempdir. If this fails the spike is dead before day 2 risk testing begins.

use spike_e1_versioned_store::SpikeStore;
use tempfile::tempdir;

#[test]
fn op_1_init_creates_both_repos() {
    let tmp = tempdir().unwrap();
    let _store = SpikeStore::init(tmp.path()).unwrap();

    assert!(tmp.path().join(".git").exists(), "knowledge .git missing");
    assert!(
        tmp.path().join("agents/.git").exists(),
        "agents .git missing"
    );
    assert!(tmp.path().join(".gitignore").exists());
    assert!(tmp.path().join("agents/.gitignore").exists());
}

#[test]
fn op_2_save_pattern_first_time_writes_v1() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    let rev = store
        .save_pattern("foo", "name: foo\ncontent: hello\n", "first write")
        .unwrap();

    assert_eq!(rev.version, 1);
    assert!(rev.archived_as.is_none(), "first write should not archive");
    assert_eq!(rev.sha.len(), 12, "expected 12-char short sha");

    let read_back = store.read_pattern("foo").unwrap().unwrap();
    assert!(read_back.contains("hello"));
}

#[test]
fn op_3_save_pattern_diff_archives_and_bumps_to_v2() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    store
        .save_pattern("foo", "version-a", "first")
        .unwrap();
    let rev2 = store
        .save_pattern("foo", "version-b", "edit")
        .unwrap();

    assert_eq!(rev2.version, 2);
    let archive = rev2.archived_as.expect("v2 should archive v1");
    assert!(tmp.path().join(&archive).exists());
    let archived = std::fs::read_to_string(tmp.path().join(&archive)).unwrap();
    assert_eq!(archived, "version-a", "archive should preserve old content");
}

#[test]
fn op_3b_save_pattern_no_diff_is_noop() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    let rev1 = store
        .save_pattern("foo", "same", "first")
        .unwrap();
    let rev2 = store
        .save_pattern("foo", "same", "same write")
        .unwrap();

    assert_eq!(
        rev1.version, rev2.version,
        "identical content should not bump version"
    );
    assert!(rev2.archived_as.is_none());
}

#[test]
fn op_4_history_returns_chrono_order_with_versions() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    store.save_pattern("foo", "a", "first").unwrap();
    store.save_pattern("foo", "b", "second").unwrap();
    store.save_pattern("foo", "c", "third").unwrap();

    let h = store.history("foo").unwrap();
    assert_eq!(h.len(), 3, "expected 3 history entries");
    assert_eq!(h[0].version, 1);
    assert_eq!(h[1].version, 2);
    assert_eq!(h[2].version, 3);
    assert!(h[0].message.contains("first"));
    assert!(h[2].message.contains("third"));
    assert!(
        h[0].timestamp <= h[2].timestamp,
        "timestamps should be non-decreasing"
    );
}

#[test]
fn op_5_rollback_creates_new_commit_with_restored_content() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    store.save_pattern("foo", "good", "first").unwrap();
    store.save_pattern("foo", "BAD EDIT", "regression").unwrap();

    let rev3 = store.rollback_pattern("foo", 1).unwrap();
    assert_eq!(rev3.version, 3, "rollback should be a new revision, not v1");

    let now = store.read_pattern("foo").unwrap().unwrap();
    assert_eq!(now, "good");

    let h = store.history("foo").unwrap();
    assert_eq!(h.len(), 3);
    assert!(
        h[2].message.contains("rollback to v1"),
        "got: {:?}",
        h[2].message
    );
}

#[test]
fn op_6_detect_external_change_after_index_drift() {
    let tmp = tempdir().unwrap();
    let mut store = SpikeStore::init(tmp.path()).unwrap();

    store.save_pattern("foo", "a", "first").unwrap();
    store.rebuild_index().unwrap();
    assert!(!store.detect_external_change().unwrap());

    // Simulate external git op: write a stale .mur-versions.yaml
    std::fs::write(
        tmp.path().join(".mur-versions.yaml"),
        "knowledge_head: deadbeef\nagents_head: cafebabe\n",
    )
    .unwrap();
    assert!(
        store.detect_external_change().unwrap(),
        "stale index should be detected"
    );

    store.rebuild_index().unwrap();
    assert!(!store.detect_external_change().unwrap());
}
