//! Inbox scanner — produces a Vec<BridgeEvent> from a directory.

use mur_agent_gui_lib::companion_bridge::scanner::scan_pending;
use tempfile::TempDir;

fn copy_fixture(into: &std::path::Path, name: &str) {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/companion-inbox")
        .join(name);
    std::fs::copy(&src, into.join(name)).expect("copy fixture");
}

#[test]
fn empty_dir_returns_empty_vec() {
    let dir = TempDir::new().unwrap();
    let out = scan_pending(dir.path()).unwrap();
    assert!(out.is_empty());
}

#[test]
fn missing_dir_returns_empty_vec_not_error() {
    let dir = TempDir::new().unwrap();
    let out = scan_pending(&dir.path().join("does-not-exist")).unwrap();
    assert!(out.is_empty());
}

#[test]
fn happy_path_returns_one_per_md_file_sorted_by_generated_at() {
    let dir = TempDir::new().unwrap();
    copy_fixture(dir.path(), "pending-warm.md");
    copy_fixture(dir.path(), "acked-good.md");

    let out = scan_pending(dir.path()).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].id, "01HPENDING_WARM_001"); // 07:13 < 10:00
    assert_eq!(out[1].id, "01HACKED_GOOD_001");
}

#[test]
fn malformed_file_is_skipped_with_warning() {
    let dir = TempDir::new().unwrap();
    copy_fixture(dir.path(), "pending-warm.md");
    copy_fixture(dir.path(), "malformed.md");

    let out = scan_pending(dir.path()).unwrap();
    assert_eq!(out.len(), 1, "malformed must be skipped, kept good one");
    assert_eq!(out[0].id, "01HPENDING_WARM_001");
}

#[test]
fn non_md_files_are_ignored() {
    let dir = TempDir::new().unwrap();
    copy_fixture(dir.path(), "pending-warm.md");
    std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
    let out = scan_pending(dir.path()).unwrap();
    assert_eq!(out.len(), 1);
}
