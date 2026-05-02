//! BridgeState + Tauri command shapes.

use mur_agent_gui_lib::companion_bridge::commands::companion_bridge_pending_inner;
use tempfile::TempDir;

#[test]
fn pending_returns_scan_results_for_existing_inbox() {
    let dir = TempDir::new().unwrap();
    let inbox = dir.path().join("agents/alex/companion/inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let path = inbox.join("01HSTATE_001.md");
    std::fs::write(
        &path,
        "\
---
id: 01HSTATE_001
situation: morning_greeting
template_id: t
locale: en
generated_at: 2026-05-02T07:00:00Z
---

hi

>>> response: <unset>",
    )
    .unwrap();

    let out = companion_bridge_pending_inner(dir.path(), "alex").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, "01HSTATE_001");
}

#[test]
fn pending_returns_empty_when_agent_dir_missing() {
    let dir = TempDir::new().unwrap();
    let out = companion_bridge_pending_inner(dir.path(), "ghost").unwrap();
    assert!(out.is_empty());
}
