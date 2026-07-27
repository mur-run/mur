//! Rule 11: MCP binary signature verification refuses startup.
//!
//! On macOS/Windows an unsigned binary must abort the agent's startup. On
//! Linux the check is a documented no-op.
//!
//! These call [`verify_mcp_supply_chain`] directly: rules 6 and 11 moved out
//! of `B0SafetyHook::on_startup` because that phase discards hook errors into
//! warnings, so neither rule could actually refuse a startup (#791).

use mur_agent_runtime::hooks::b0::verify_mcp_supply_chain;
use mur_common::AgentProfile;
use tempfile::TempDir;

fn minimal_profile() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_minimal.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parse")
}

#[cfg(target_os = "macos")]
#[test]
fn unsigned_mcp_binary_fails_startup() {
    let dir = TempDir::new().unwrap();
    // Create an unsigned executable (just a small file).
    let bin = dir.path().join("fake-mcp");
    std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&bin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).unwrap();

    let err = verify_mcp_supply_chain(&[bin], &minimal_profile())
        .expect_err("unsigned binary should refuse startup");
    let lower = err.to_lowercase();
    assert!(
        lower.contains("not signed") || lower.contains("signing") || lower.contains("signature"),
        "expected signature-related error, got {err}",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_signature_check_is_a_noop() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("any");
    std::fs::write(&bin, b"x").unwrap();
    assert!(
        verify_mcp_supply_chain(&[bin], &minimal_profile()).is_ok(),
        "linux signature check should be a noop"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_unsigned_binary_fails_startup() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("fake-mcp.exe");
    std::fs::write(&bin, b"MZ\0\0").unwrap();
    let err = verify_mcp_supply_chain(&[bin], &minimal_profile())
        .expect_err("unsigned windows binary should refuse startup");
    assert!(
        err.to_lowercase().contains("not signed") || err.contains("signtool"),
        "got {err}"
    );
}
