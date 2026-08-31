//! `BridgeBeacon` — a 30 s heartbeat for LLM-less bridge agents.
//!
//! Each bridge agent (one whose `entitlements.llm.mode = off`) emits a
//! `telemetry/bridge_alive` JSON-RPC notification every 30 s. Peers that
//! cooperate with the bridge (typically a user agent reading the
//! commander or another agent's notification stream) classify the
//! bridge's liveness via [`bridge_status_for_peer`].
//!
//! That used to read `running.lock`'s mtime, on the belief that writing
//! telemetry advanced it. Writing telemetry advances the *telemetry file's*
//! mtime; `running.lock` is written once at startup and never again, so the
//! age being measured was simply the bridge's uptime and every bridge read as
//! `Degraded` ninety seconds after starting. The intent was right and the file
//! was wrong — see issue #1085.
//!
//! See plan task M-c1.4 in
//! `docs/superpowers/plans/2026-05-03-mur-agent-track-c1-a2a-bridge.md`.

use std::path::Path;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::Sender;

use crate::telemetry_writer::Event;

pub use mur_common::telemetry::METHOD_BRIDGE_ALIVE;

/// Maximum age of a bridge's last written telemetry before peers should
/// treat the bridge as `Degraded`. The bridge supervisor emits a
/// `telemetry/bridge_alive` notification every 30 s, so a 90 s window
/// tolerates two missed beats.
pub const DEGRADED_AFTER_SECS: u64 = 90;

/// Build the JSON-RPC notification body emitted by [`BridgeBeacon`].
///
/// Pure function exposed for tests and downstream tooling that needs to
/// inspect / replay the on-wire shape without a live channel.
pub fn make_alive_payload(bridge_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": METHOD_BRIDGE_ALIVE,
        "params": {
            "bridge_id": bridge_id,
            "ts": chrono::Utc::now().to_rfc3339(),
        },
    }))
    .expect("static JSON serializes")
}

/// Periodic heartbeat emitter for a bridge agent.
///
/// Construct one per supervisor and call [`BridgeBeacon::spawn`] to
/// drive a background task. The task exits cleanly when the telemetry
/// channel is closed (i.e. on supervisor shutdown).
pub struct BridgeBeacon {
    bridge_id: String,
    tx: Sender<Event>,
    interval: Duration,
}

impl BridgeBeacon {
    /// Default 30 s heartbeat interval per spec §M-c1.4.1.
    pub fn new(bridge_id: impl Into<String>, tx: Sender<Event>) -> Self {
        Self {
            bridge_id: bridge_id.into(),
            tx,
            interval: Duration::from_secs(30),
        }
    }

    /// Spawn the heartbeat loop. Emits an `Event::BridgeAlive` event on
    /// every tick; the runtime's `TelemetryWriter` translates that into
    /// a `telemetry/bridge_alive` JSON-RPC notification matching
    /// [`make_alive_payload`].
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut t = tokio::time::interval(self.interval);
            // First tick fires immediately; we want to wait one full
            // interval before the first beat so the supervisor has time
            // to fully attach transports.
            t.tick().await;
            loop {
                t.tick().await;
                let ev = Event::BridgeAlive {
                    bridge_id: self.bridge_id.clone(),
                };
                if self.tx.send(ev).await.is_err() {
                    break;
                }
            }
        })
    }
}

/// Coarse liveness classification for a bridge peer.
///
/// Returned by [`bridge_status_for_peer`]; consumed by `mur agent
/// doctor`'s `bridges:` section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgePeerStatus {
    /// `running.lock` exists and was modified within the last
    /// [`DEGRADED_AFTER_SECS`] seconds.
    Running,
    /// `running.lock` exists but its mtime is older than
    /// [`DEGRADED_AFTER_SECS`] seconds (heartbeat stalled).
    Degraded,
    /// `running.lock` is missing or unreadable.
    Offline,
}

/// Classify a bridge peer by inspecting `running.lock` mtime in
/// `agent_dir` (typically `~/.mur/agents/<name>/`).
pub fn bridge_status_for_peer(agent_dir: &Path) -> BridgePeerStatus {
    // Existence comes from the lock, which is accurate: it is written at
    // startup and removed on shutdown. Recency comes from telemetry, which is
    // the file a beat actually touches.
    let lock = match std::fs::metadata(agent_dir.join("running.lock")).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return BridgePeerStatus::Offline,
    };
    // The lock's mtime is the start time, and it stays useful as a floor: the
    // first beat only lands after one full interval, so a bridge that started
    // twenty seconds ago has no telemetry yet and is not degraded.
    let last_sign_of_life = newest_telemetry_write(agent_dir)
        .max(Some(lock))
        .unwrap_or(lock);
    let age = SystemTime::now()
        .duration_since(last_sign_of_life)
        .unwrap_or_default()
        .as_secs();
    if age > DEGRADED_AFTER_SECS {
        BridgePeerStatus::Degraded
    } else {
        BridgePeerStatus::Running
    }
}

/// When this agent last appended telemetry.
///
/// One file per day (`telemetry/YYYY-MM-DD.jsonl`), appended per event, so the
/// newest mtime across them is the last time the process wrote anything —
/// which for a bridge agent is dominated by its own heartbeat.
fn newest_telemetry_write(agent_dir: &Path) -> Option<SystemTime> {
    std::fs::read_dir(agent_dir.join("telemetry"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

#[cfg(test)]
mod tests {
    fn stamp(p: &std::path::Path, ago: u64) {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(p, "x").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(ago))
            .unwrap();
    }

    /// The bug in #1085: a bridge beating normally read `Degraded` because the
    /// age being measured was its uptime. Old lock, fresh telemetry — that is
    /// a healthy bridge, and the only thing distinguishing it is which file is
    /// consulted.
    #[test]
    fn a_bridge_up_for_hours_and_still_beating_is_running() {
        let d = tempfile::tempdir().unwrap();
        stamp(&d.path().join("running.lock"), 7 * 3600);
        stamp(&d.path().join("telemetry/2026-08-31.jsonl"), 10);
        assert_eq!(bridge_status_for_peer(d.path()), BridgePeerStatus::Running);
    }

    #[test]
    fn a_bridge_that_stopped_beating_is_degraded() {
        let d = tempfile::tempdir().unwrap();
        stamp(&d.path().join("running.lock"), 7 * 3600);
        stamp(
            &d.path().join("telemetry/2026-08-31.jsonl"),
            DEGRADED_AFTER_SECS + 60,
        );
        assert_eq!(bridge_status_for_peer(d.path()), BridgePeerStatus::Degraded);
    }

    /// The first beat only lands after one full interval, so a bridge that
    /// started seconds ago has no telemetry and must not read as degraded.
    #[test]
    fn a_bridge_that_just_started_is_running_before_its_first_beat() {
        let d = tempfile::tempdir().unwrap();
        stamp(&d.path().join("running.lock"), 5);
        std::fs::create_dir_all(d.path().join("telemetry")).unwrap();
        assert_eq!(bridge_status_for_peer(d.path()), BridgePeerStatus::Running);
    }

    /// …but a long-running process that has never written anything is not
    /// healthy, and the lock's mtime must not keep it looking alive forever.
    #[test]
    fn silence_for_longer_than_the_window_is_degraded_even_with_a_lock() {
        let d = tempfile::tempdir().unwrap();
        stamp(&d.path().join("running.lock"), DEGRADED_AFTER_SECS + 60);
        assert_eq!(bridge_status_for_peer(d.path()), BridgePeerStatus::Degraded);
    }

    #[test]
    fn no_lock_is_offline_whatever_telemetry_says() {
        let d = tempfile::tempdir().unwrap();
        stamp(&d.path().join("telemetry/2026-08-31.jsonl"), 1);
        assert_eq!(bridge_status_for_peer(d.path()), BridgePeerStatus::Offline);
    }

    use super::*;
    #[test]
    fn payload_has_method_and_bridge_id() {
        let p = make_alive_payload("bridge_telegram");
        let v: serde_json::Value = serde_json::from_slice(&p).unwrap();
        assert_eq!(v["method"], "telemetry/bridge_alive");
        assert_eq!(v["params"]["bridge_id"], "bridge_telegram");
    }
}
