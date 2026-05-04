//! Integration test for `mur agent doctor` — `bridges:` section.
//!
//! Plan task M-c1.4.4 — verifies that
//! [`mur_core::cmd::doctor::collect_bridge_statuses`] enumerates
//! agents whose `entitlements.llm.mode = off` and classifies each by
//! `running.lock` mtime via `bridge_status_for_peer`.

use mur_agent_runtime::bridge::beacon::BridgePeerStatus;
use mur_core::cmd::doctor::{BridgeSummary, collect_bridge_statuses};
use tempfile::TempDir;

fn write_bridge_fixture(dir: &std::path::Path) {
    // Minimal AgentProfile YAML w/ entitlements.llm.mode = off. Adapted
    // from the round-trip fixture at mur-common/src/agent.rs:691 with
    // `entitlements.llm: { mode: off }` injected. The profile's own
    // `name:` is irrelevant — `collect_bridge_statuses` keys results
    // off the on-disk directory name (matching `~/.mur/agents/<name>/`).
    std::fs::write(
        dir.join("profile.yaml"),
        include_str!("fixtures/bridge_profile.yaml"),
    )
    .unwrap();
}

#[test]
fn lists_running_and_degraded() {
    let tmp = TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    let a = agents.join("bridge_a");
    let b = agents.join("bridge_b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    write_bridge_fixture(&a);
    write_bridge_fixture(&b);
    std::fs::write(a.join("running.lock"), b"{}").unwrap();
    let stale = b.join("running.lock");
    std::fs::write(&stale, b"{}").unwrap();
    // Windows requires write access to set mtime; std::fs::File::open is read-only.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
        .unwrap();

    let summary = collect_bridge_statuses(tmp.path());
    let map: std::collections::BTreeMap<_, _> = summary
        .iter()
        .map(|s: &BridgeSummary| (s.name.clone(), s.status))
        .collect();
    assert_eq!(map["bridge_a"], BridgePeerStatus::Running);
    assert_eq!(map["bridge_b"], BridgePeerStatus::Degraded);
}
