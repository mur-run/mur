//! InboxWatcher — emits BridgeEvent on every new .md write.

use mur_agent_gui_lib::companion_bridge::watcher::InboxWatcher;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;

const FIXTURE_BODY: &str = "\
---
id: 01HWATCHER_TEST_001
situation: morning_greeting
template_id: greet_warm_zh_001
locale: zh-TW
generated_at: 2026-05-02T07:13:03+08:00
---

早安。

>>> response: <unset>";

#[tokio::test]
async fn writing_a_new_md_file_emits_one_event() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let watcher = InboxWatcher::start(dir.path().to_path_buf(), tx).expect("watcher start");
    tokio::time::sleep(Duration::from_millis(150)).await;
    std::fs::write(dir.path().join("01HWATCHER_TEST_001.md"), FIXTURE_BODY).unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("watcher must emit within 2s")
        .expect("channel still open");
    assert_eq!(ev.id, "01HWATCHER_TEST_001");
    drop(watcher);
}

#[tokio::test]
async fn malformed_file_does_not_emit_and_does_not_crash() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let _watcher = InboxWatcher::start(dir.path().to_path_buf(), tx).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    std::fs::write(dir.path().join("garbage.md"), "no front matter here").unwrap();
    let result = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
    assert!(result.is_err(), "malformed file must not produce an event");
}
