use super::*;
use serde_json::json;

#[test]
fn ack_query_skips() {
    assert!(should_skip(Some("ok")));
    assert!(should_skip(Some("好")));
    assert!(should_skip(Some("thanks")));
}

#[test]
fn empty_query_skips() {
    assert!(should_skip(None));
    assert!(should_skip(Some("")));
    assert!(should_skip(Some("   ")));
}

#[test]
fn coding_query_does_not_skip() {
    assert!(!should_skip(Some(
        "refactor the token budget enforcement to support per-tier caps"
    )));
    assert!(!should_skip(Some(
        "implement retry logic with exponential backoff"
    )));
}

#[test]
fn extract_query_from_claude_raw() {
    let raw = json!({"prompt": "implement error retry", "session_id": "s1"});
    assert_eq!(
        extract_query(&raw).as_deref(),
        Some("implement error retry")
    );
}

#[test]
fn extract_query_missing_returns_none() {
    let raw = json!({"tool_name": "Edit"});
    assert!(extract_query(&raw).is_none());
}

#[test]
fn superpowers_detection_and_suppression_matrix() {
    use mur_common::config::DevDisciplineIndex as D;
    let home = tempfile::tempdir().unwrap();
    // No plugin dirs at all → not present.
    assert!(!super::superpowers_plugin_present(home.path()));
    assert!(!super::dev_hub_suppressed(D::Auto, home.path()));
    // Marker dir two levels under plugins/cache → present.
    let plug = home
        .path()
        .join(".claude/plugins/cache/claude-plugins-official/superpowers");
    std::fs::create_dir_all(&plug).unwrap();
    assert!(super::superpowers_plugin_present(home.path()));
    assert!(super::dev_hub_suppressed(D::Auto, home.path()));
    // Config overrides beat detection.
    assert!(!super::dev_hub_suppressed(D::Always, home.path()));
    assert!(super::dev_hub_suppressed(D::Never, home.path()));
    // `Never` suppresses even without a plugin present.
    let empty = tempfile::tempdir().unwrap();
    assert!(super::dev_hub_suppressed(D::Never, empty.path()));
}
