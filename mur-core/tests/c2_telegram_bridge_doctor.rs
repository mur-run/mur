//! Integration test for `mur agent doctor` — Track C2 telegram bridge.
//!
//! Plan task M-c2.6.3 — verifies that the supervisor's
//! `BridgeBeacon` spawn path (via
//! [`mur_agent_runtime::supervisor::spawn_telegram_bridge_for_test`])
//! produces a `running.lock` that
//! [`mur_core::cmd::doctor::collect_bridge_statuses`] classifies as
//! `Running`. This is the user-visible end-to-end the
//! `bridges:` section of `mur agent doctor` reports.

use mur_agent_runtime::bridge::beacon::BridgePeerStatus;
use mur_agent_runtime::supervisor::spawn_telegram_bridge_for_test;
use mur_core::cmd::doctor::collect_bridge_statuses;
use tempfile::TempDir;

fn write_telegram_bridge_profile(dir: &std::path::Path) {
    std::fs::write(
        dir.join("profile.yaml"),
        // Mirror of mur-agent-runtime/tests/fixtures/bridge_profile_telegram.yaml.
        // Inlined here so this test crate doesn't reach into a sibling
        // crate's tests/fixtures (the file would not be packaged with
        // the runtime crate either).
        include_str!("fixtures/bridge_profile_telegram.yaml"),
    )
    .unwrap();
}

#[tokio::test]
async fn doctor_reports_telegram_bridge_running_within_5s() {
    let mur_home = TempDir::new().unwrap();
    let agents = mur_home.path().join("agents");
    let bridge_dir = agents.join("tg_bridge");
    std::fs::create_dir_all(&bridge_dir).unwrap();
    write_telegram_bridge_profile(&bridge_dir);

    // Drive the same bridge-side spawn path the real supervisor takes
    // when entitlements.llm.mode = off: instantiate a TelemetryWriter,
    // spawn BridgeBeacon, write a fresh running.lock.
    let handle = spawn_telegram_bridge_for_test(&bridge_dir).await.unwrap();

    // The doctor walk reads `<mur_home>/agents/*/profile.yaml` then
    // `bridge_status_for_peer(<dir>)` of each. We don't need to wait
    // for an actual heartbeat tick — the running.lock written by
    // spawn_telegram_bridge_for_test is fresh, which is what the
    // peer-side classifier looks at.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let summary = collect_bridge_statuses(mur_home.path());
    assert_eq!(summary.len(), 1, "expected exactly one bridge entry");
    assert_eq!(summary[0].name, "tg_bridge");
    assert_eq!(summary[0].status, BridgePeerStatus::Running);

    handle.shutdown().await;
}
