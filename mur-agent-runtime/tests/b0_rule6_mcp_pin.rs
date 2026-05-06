//! B0 rule 6 / M9.3 — `B0SafetyHook::on_startup` enforces install-time
//! MCP binary pin.
//!
//! Construction follows the same pattern as `b0_rule11_mcp_signature.rs`:
//! load a minimal `AgentProfile`, inject a synthetic `mcp_servers`
//! entry whose `binary_sha256` either matches or differs from the
//! actual file on disk, then assert `on_startup` outcome.
//!
//! Linux-only because on macOS rule 11 (codesign check) fires before
//! rule 6 and would refuse the unsigned synthetic binary. The unit
//! tests in `hooks::b0_helpers::pin_verify_tests` cover the verifier
//! itself cross-platform.

#![cfg(target_os = "linux")]

use mur_agent_runtime::hooks::{B0SafetyHook, Hook, HookCtx};
use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn minimal_profile() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_minimal.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parse")
}

fn write_fake_binary(dir: &std::path::Path, contents: &[u8]) -> PathBuf {
    // Use a unique name in case multiple tests share the same TempDir.
    let bin = dir.join(format!("fake-mcp-{}", uuid::Uuid::now_v7()));
    std::fs::write(&bin, contents).unwrap();
    bin
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// On Linux the rule 11 signature check is a no-op so we can land
/// pinned-but-unsigned binaries cleanly. On macOS the rule 11
/// codesign check fires before rule 6, so this rule-6 test would
/// require a signed binary fixture — gate it to Linux only for now.
/// (The unit tests in `hooks::b0_helpers::pin_verify_tests` cover
/// the verifier itself cross-platform.)
#[cfg(target_os = "linux")]
#[tokio::test]
async fn matching_binary_hash_passes_startup() {
    let dir = TempDir::new().unwrap();
    let payload = b"pretend MCP body v1\n";
    let bin = write_fake_binary(dir.path(), payload);
    let pinned_hash = sha256_hex(payload);

    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: "weather".into(),
        command: bin.display().to_string(),
        args: vec![],
        binary_sha256: Some(pinned_hash),
        ..Default::default()
    });

    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![bin.clone()]);
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    assert!(result.is_ok(), "matching pin should pass: {result:?}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn binary_drift_fails_startup_with_inspect_hint() {
    let dir = TempDir::new().unwrap();
    let installed = b"pretend MCP body v1\n";
    let bin = write_fake_binary(dir.path(), installed);
    // Pin records v1's hash; rewrite the file to v2's bytes.
    let pinned_hash = sha256_hex(installed);
    std::fs::write(&bin, b"pretend MCP body v2 -- drifted\n").unwrap();

    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: "weather".into(),
        command: bin.display().to_string(),
        args: vec![],
        binary_sha256: Some(pinned_hash),
        ..Default::default()
    });

    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![bin]);
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    let err = result.expect_err("drift should fail startup");
    let msg = err.to_string();
    assert!(msg.contains("B0 rule 6"), "got {msg}");
    assert!(msg.contains("mur agent mcp inspect weather"), "got {msg}");
    assert!(msg.contains("mur agent mcp pin weather"), "got {msg}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn missing_binary_soft_fails_does_not_block_startup() {
    let dir = TempDir::new().unwrap();
    // Path that doesn't exist — the supervisor would catch this in
    // its own resolve step in production, but the verify path must
    // not crash on a missing file (user uninstalled the MCP without
    // removing it from profile.yaml).
    let phantom = dir.path().join("phantom-mcp");

    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: "ghost".into(),
        command: phantom.display().to_string(),
        args: vec![],
        binary_sha256: Some("deadbeef".repeat(8)),
        ..Default::default()
    });

    let hook = B0SafetyHook::new();
    // Rule 11 reads `ctx.mcp_server_binaries()` (resolved by the
    // supervisor — production already filters out unresolvable paths
    // before the hook runs). Rule 6 reads `profile.mcp_servers`. To
    // exercise rule 6's soft-fail path in isolation, we pass an
    // empty mcp_server_binaries list — rule 11 then has nothing to
    // verify, and rule 6 alone sees the phantom entry from the
    // profile.
    let _ = phantom;
    let ctx = HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![]);
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    assert!(
        result.is_ok(),
        "missing binary should soft-fail (warn, allow start): {result:?}",
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn pre_m9_entry_without_pin_skipped_cleanly() {
    let dir = TempDir::new().unwrap();
    let bin = write_fake_binary(dir.path(), b"any bytes");

    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: "legacy".into(),
        command: bin.display().to_string(),
        args: vec![],
        // binary_sha256 stays None — pre-M9 entry, exempt from rule 6.
        ..Default::default()
    });

    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![bin]);
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    assert!(
        result.is_ok(),
        "pre-M9 (no pin) entry should pass: {result:?}",
    );
}
