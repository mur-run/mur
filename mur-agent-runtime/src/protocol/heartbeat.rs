//! `turn/heartbeat` — proof of life while a turn runs (spec 2026-09-12
//! execution-limits §3.6, D7). The dial on the other end keeps a SHORT idle
//! timeout and never fires on a router that is merely thinking, because a
//! frame arrives every thirty seconds whether or not the model has produced
//! a token. Liveness is this frame; a bigger read timeout would be the same
//! failure, later.

use std::time::Duration;

use serde_json::{Value, json};

/// Half of the 60 s the spec allows between beats; three missed beats are
/// the dial's 90 s.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const HEARTBEAT_METHOD: &str = "turn/heartbeat";

/// Stops the beat when dropped — hold it for exactly the turn's lifetime.
pub struct HeartbeatGuard {
    pub(crate) handle: tokio::task::JoinHandle<()>,
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Beat on `notifier` every `every` until dropped or the connection closes.
/// The first beat is one interval in — a turn shorter than that never beats,
/// which is fine: its reply arrives first.
pub fn spawn(
    notifier: tokio::sync::mpsc::Sender<Value>,
    task_id: String,
    every: Duration,
) -> HeartbeatGuard {
    let handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // the immediate first tick
        loop {
            tick.tick().await;
            let frame = json!({
                "jsonrpc": "2.0",
                "method": HEARTBEAT_METHOD,
                "params": { "task_id": task_id, "at": chrono::Utc::now().to_rfc3339() },
            });
            if notifier.send(frame).await.is_err() {
                break; // connection gone — nobody to reassure
            }
        }
    });
    HeartbeatGuard { handle }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Frames arrive on the interval, carry the task id, and stop when the
    /// guard is dropped — the turn's end is the last beat.
    #[tokio::test]
    async fn beats_on_the_interval_and_stops_with_the_guard() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let guard = spawn(tx, "t-1".into(), std::time::Duration::from_millis(50));
        let mut seen = 0;
        while seen < 3 {
            let f = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("a beat within 2s")
                .expect("channel open");
            assert_eq!(f["method"], HEARTBEAT_METHOD);
            assert_eq!(f["params"]["task_id"], "t-1");
            assert!(
                f["params"]["at"].as_str().unwrap().contains('T'),
                "rfc3339: {f}"
            );
            assert!(f.get("id").is_none(), "a heartbeat is a notification");
            seen += 1;
        }
        drop(guard);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(rx.try_recv().is_err(), "no beats after the guard is gone");
    }

    /// A closed connection ends the task instead of logging forever.
    #[tokio::test]
    async fn a_closed_sink_ends_the_task() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let guard = spawn(tx, "t-2".into(), std::time::Duration::from_millis(10));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(guard.handle.is_finished());
    }
}
