//! Test-only bridge beacon helpers used by integration tests.

use super::*;

/// Test-only helper: spawn just the bridge-side of the supervisor for a
/// telegram bridge agent rooted at `agent_dir`.
///
/// Drives the same code path the real supervisor takes when
/// `entitlements.llm.mode = off` — instantiates a [`TelemetryWriter`]
/// pointed at `agent_dir/telemetry/`, spawns a [`crate::bridge::beacon::BridgeBeacon`]
/// keyed on `bridge_id` (defaults to `"tg"`), and writes a fresh
/// `running.lock` so peers' `bridge_status_for_peer` classifies the
/// agent as `Running` immediately.
///
/// Used by M-c2.6.3 and downstream `mur agent doctor` integration tests
/// to verify the heartbeat path is wired without spinning up a full
/// runtime (no telegram bot token, no MCP child, no transport listener).
///
/// Returns a [`BridgeTestHandle`]; call `.shutdown().await` to abort
/// the beacon task and remove `running.lock`.
pub async fn spawn_telegram_bridge_for_test(
    agent_dir: &std::path::Path,
) -> std::io::Result<BridgeTestHandle> {
    spawn_bridge_for_test_with_id(agent_dir, "tg").await
}

/// Lower-level form of [`spawn_telegram_bridge_for_test`] that lets
/// callers pick a custom `bridge_id` (the value embedded in
/// `telemetry/bridge_alive` payloads).
pub async fn spawn_bridge_for_test_with_id(
    agent_dir: &std::path::Path,
    bridge_id: &str,
) -> std::io::Result<BridgeTestHandle> {
    std::fs::create_dir_all(agent_dir)?;
    let lock_path = agent_dir.join("running.lock");
    std::fs::write(&lock_path, b"{}")?;

    let telemetry_dir = agent_dir.join("telemetry");
    let (writer, _notif_rx) = TelemetryWriter::new(
        telemetry_dir,
        bridge_id.to_string(),
        format!("test-{bridge_id}"),
    )
    .await?;

    let beacon = crate::bridge::beacon::BridgeBeacon::new(bridge_id.to_string(), writer.sender());
    let join = beacon.spawn();

    Ok(BridgeTestHandle {
        beacon: Some(join),
        lock_path,
    })
}

/// Handle returned by [`spawn_telegram_bridge_for_test`]. Aborts the
/// background heartbeat task and tidies `running.lock` on
/// [`BridgeTestHandle::shutdown`] (or on drop).
pub struct BridgeTestHandle {
    beacon: Option<tokio::task::JoinHandle<()>>,
    lock_path: PathBuf,
}

impl BridgeTestHandle {
    /// Abort the heartbeat task and remove `running.lock`. Idempotent.
    pub async fn shutdown(mut self) {
        if let Some(j) = self.beacon.take() {
            j.abort();
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

impl Drop for BridgeTestHandle {
    fn drop(&mut self) {
        if let Some(j) = self.beacon.take() {
            j.abort();
        }
        let _ = std::fs::remove_file(&self.lock_path);
    }
}
