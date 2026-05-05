//! Tests the inbox read helpers used by mur hook prompt.

use mur_core::daemon::{inbox_path, read_inbox};

#[test]
fn fresh_inbox_is_returned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_session.md");
    std::fs::write(&path, "## mur context\n- foo — bar\n").unwrap();
    let content = read_inbox(&path, 300).unwrap();
    assert!(content.contains("mur context"));
}

#[test]
fn missing_inbox_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.md");
    assert!(read_inbox(&path, 300).is_none());
}

#[test]
fn stale_inbox_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.md");
    std::fs::write(&path, "old content").unwrap();
    assert!(read_inbox(&path, 0).is_none());
}

#[test]
fn inbox_path_includes_session_id() {
    let p = inbox_path("my-session-123");
    assert!(p.to_string_lossy().contains("my-session-123"));
    assert!(p.to_string_lossy().ends_with(".md"));
}
