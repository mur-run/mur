//! Rule 11: on_startup verifies MCP binary signatures.
//!
//! On macOS, an unsigned binary in profile.mcp_servers triggers a
//! HookError. On Linux this test is a no-op (rule doesn't apply).

use mur_agent_runtime::hooks::{B0SafetyHook, Hook, HookCtx};
use mur_common::AgentProfile;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn minimal_profile() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_minimal.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parse")
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn unsigned_mcp_binary_fails_startup() {
    let dir = TempDir::new().unwrap();
    // Create an unsigned executable (just a small file).
    let bin = dir.path().join("fake-mcp");
    std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();

    let hook = B0SafetyHook::new();
    let ctx =
        HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![bin.to_path_buf()]);
    let profile = minimal_profile();
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    assert!(result.is_err(), "unsigned binary should fail on_startup");
    let msg = format!("{}", result.unwrap_err());
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("not signed") || lower.contains("signing") || lower.contains("signature"),
        "expected signature-related error, got {msg}",
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_signature_check_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("any");
    std::fs::write(&bin, b"x").unwrap();
    let hook = B0SafetyHook::new();
    let ctx =
        HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![bin.to_path_buf()]);
    let profile = minimal_profile();
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    assert!(result.is_ok(), "linux signature check should be noop");
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn windows_unsigned_binary_fails_startup() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("fake-mcp.exe");
    std::fs::write(&bin, b"MZ\0\0").unwrap();
    let hook = B0SafetyHook::new();
    let ctx =
        HookCtx::for_test_with_mcp_servers(dir.path().to_path_buf(), 1, vec![bin.to_path_buf()]);
    let profile = minimal_profile();
    let cancel = CancellationToken::new();
    let result = hook.on_startup(&ctx, &profile, &cancel).await;
    assert!(
        result.is_err(),
        "unsigned windows binary should fail on_startup"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.to_lowercase().contains("not signed") || msg.contains("signtool"),
        "got {msg}"
    );
}
