//! `BridgeBeacon` — a 30 s heartbeat for LLM-less bridge agents.
//!
//! Each bridge agent (one whose `entitlements.llm.mode = off`) emits a
//! `telemetry/bridge_alive` JSON-RPC notification every 30 s. Peers that
//! cooperate with the bridge (typically a user agent reading the
//! commander or another agent's notification stream) classify the
//! bridge's liveness via [`bridge_status_for_peer`] which inspects the
//! `running.lock` mtime — a still-running supervisor refreshes the lock
//! on every beacon emit (file mtime advances when telemetry is written
//! through `TelemetryWriter`'s JSONL append path).
//!
//! See plan task M-c1.4 in
//! `docs/superpowers/plans/2026-05-03-mur-agent-track-c1-a2a-bridge.md`.

use std::time::Duration;
use tokio::sync::mpsc::Sender;

use crate::telemetry_writer::Event;

pub use mur_common::telemetry::METHOD_BRIDGE_ALIVE;

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_has_method_and_bridge_id() {
        let p = make_alive_payload("bridge_telegram");
        let v: serde_json::Value = serde_json::from_slice(&p).unwrap();
        assert_eq!(v["method"], "telemetry/bridge_alive");
        assert_eq!(v["params"]["bridge_id"], "bridge_telegram");
    }
}
