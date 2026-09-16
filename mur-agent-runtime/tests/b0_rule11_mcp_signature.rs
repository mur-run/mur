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
    // An unsigned *native image*: Mach-O magic (MH_MAGIC_64) and nothing
    // `codesign` can verify behind it.
    //
    // This fixture used to be `#!/bin/sh\nexit 0\n`, which is a script, not a
    // binary — and rule 11 refused it, which is precisely the bug that made an
    // agent with an `npx` MCP server unbootable: `npx` canonicalizes to a
    // `.js` file no signature can ever cover. The rule now enforces on
    // signable images only, so the fixture has to actually be one for this
    // test to still be testing what its name says. The skip side is covered by
    // `b0_rule11_signability.rs`.
    let bin = dir.path().join("fake-mcp");
    std::fs::write(&bin, b"\xcf\xfa\xed\xfenot-a-real-macho").unwrap();
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

/// Windows goes through `WinVerifyTrust` (`wintrust.dll`), which is present on
/// every install — unlike `signtool`, which ships with the SDK and whose
/// absence used to refuse startup for every native binary on a stock machine
/// (#1332).
///
/// What this asserts is deliberately narrow. Nobody here knows which HRESULT
/// Authenticode returns for a four-byte file claiming to be a PE, and asserting
/// a guess is how the previous version of this suite ended up asserting that
/// `cmd.exe` verifies on a runner with no SDK. The verdict *policy* — which
/// statuses refuse a startup — is pinned exhaustively and on every platform by
/// `b0_helpers::wintrust_tests`. What is left to check here is that the call
/// completes and never speaks of `signtool` again.
#[cfg(target_os = "windows")]
#[test]
fn windows_signature_check_needs_no_sdk() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("fake-mcp.exe");
    std::fs::write(&bin, b"MZ\0\0").unwrap();
    match verify_mcp_supply_chain(&[bin], &minimal_profile()) {
        Ok(()) => {}
        Err(msg) => {
            assert!(
                !msg.contains("signtool"),
                "the SDK-dependent path is gone; got {msg}"
            );
            assert!(
                msg.contains("B0 rule 11"),
                "a refusal must still name the rule: {msg}"
            );
        }
    }
}
