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

#[tokio::test]
async fn subscribe_starts_watcher_and_forwards_writes() {
    use mur_agent_gui_lib::companion_bridge::commands::companion_bridge_subscribe_inner;
    use mur_agent_gui_lib::companion_bridge::state::BridgeState;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    let dir = TempDir::new().unwrap();
    let inbox = dir.path().join("agents/alex/companion/inbox");
    std::fs::create_dir_all(&inbox).unwrap();

    let state = Arc::new(BridgeState::default());
    let (tx, mut rx) = mpsc::channel(8);
    companion_bridge_subscribe_inner(dir.path(), "alex", state.clone(), tx).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    std::fs::write(
        inbox.join("01HSUB_001.md"),
        "\
---
id: 01HSUB_001
situation: morning_greeting
template_id: t
locale: en
generated_at: 2026-05-02T07:00:00Z
---

hi

>>> response: <unset>",
    )
    .unwrap();

    let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("must receive within 2s")
        .expect("channel still open");
    assert_eq!(ev.id, "01HSUB_001");
}
