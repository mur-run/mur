//! Acceptance: from .md write to BridgeEvent on the mpsc bus must be
//! < 1 s (the spec's "≤ 1 s end-to-end" SLA, with margin for the
//! ~150 ms watcher attach + React render budget left over).

use mur_agent_gui_lib::companion_bridge::watcher::InboxWatcher;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::sync::mpsc;

const FIXTURE: &str = "\
---
id: 01HACCEPTANCE_001
situation: morning_greeting
template_id: t
locale: en
generated_at: 2026-05-02T07:00:00Z
---

hi

>>> response: <unset>";

#[tokio::test]
async fn watcher_to_event_latency_under_1s() {
    let dir = TempDir::new().unwrap();
    let (tx, mut rx) = mpsc::channel(8);
    let _w = InboxWatcher::start(dir.path().to_path_buf(), tx).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let started = Instant::now();
    std::fs::write(dir.path().join("01HACCEPTANCE_001.md"), FIXTURE).unwrap();
    let _ev = tokio::time::timeout(Duration::from_millis(1000), rx.recv())
        .await
        .expect("must arrive within 1s")
        .expect("channel still open");
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "latency exceeded 1s budget: {:?}",
        started.elapsed()
    );
}
