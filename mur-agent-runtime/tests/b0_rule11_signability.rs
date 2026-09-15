//! B0 rule 11 — signature enforcement applies to *signable native images only*,
//! and admission covers only the MCP servers the profile actually spawns.
//!
//! Both regressions come from one field report (`mur` concierge, 2026-09-15):
//! adding two `npx` MCP servers made the agent permanently unbootable with
//!
//! ```text
//! B0 rule 11: MCP binary signature check failed: macOS binary not signed:
//!   .../npm/bin/npx-cli.js
//! ```
//!
//! `npx` canonicalizes to `npx-cli.js`, a JavaScript file, so `codesign` can
//! never verify it — the check was unsatisfiable, and the documented recovery
//! (`mur agent mcp disable`) did not help because admission iterated *every*
//! entry, disabled ones included.

use mur_agent_runtime::hooks::b0::verify_mcp_supply_chain;
use mur_agent_runtime::hooks::b0_helpers::verify_signed;
use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn minimal_profile() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_minimal.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parse")
}

fn write(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, contents).unwrap();
    p
}

/// A package-runner shim: what `which npx` resolves to after `canonicalize()`.
#[test]
fn interpreter_script_is_not_a_signature_failure() {
    let dir = TempDir::new().unwrap();
    let js = write(
        dir.path(),
        "npx-cli.js",
        b"#!/usr/bin/env node\nrequire('../lib/cli.js')\n",
    );
    assert!(
        verify_signed(&js).is_ok(),
        "a JS shim is not a signable image — enforcing on it is unsatisfiable"
    );

    let profile = minimal_profile();
    let result = verify_mcp_supply_chain(&[js], &profile);
    assert!(
        result.is_ok(),
        "rule 11 must not brick an agent over an interpreter script: {result:?}"
    );
}

/// Shell wrappers are the other unsignable shape MCP entries resolve to.
#[test]
fn shell_wrapper_is_not_a_signature_failure() {
    let dir = TempDir::new().unwrap();
    let sh = write(
        dir.path(),
        "server-wrapper",
        b"#!/bin/sh\nexec node ./x.js\n",
    );
    assert!(
        verify_signed(&sh).is_ok(),
        "shebang wrapper must be skipped"
    );
}

/// The protection rule 11 exists for is unchanged: a real Mach-O / PE image is
/// still handed to the platform verifier.
#[test]
fn native_images_are_still_checked() {
    let signed = Path::new(if cfg!(windows) {
        r"C:\Windows\System32\cmd.exe"
    } else {
        "/bin/ls"
    });
    if !signed.exists() {
        return; // no platform binary to lean on
    }
    assert!(
        verify_signed(signed).is_ok(),
        "the platform's own binary must verify"
    );

    // Truncated Mach-O/PE header: claims to be a native image, cannot verify.
    let dir = TempDir::new().unwrap();
    let magic: &[u8] = if cfg!(windows) {
        b"MZ\x90\x00garbage-not-a-real-pe"
    } else {
        b"\xcf\xfa\xed\xfegarbage-not-a-real-macho"
    };
    let fake = write(dir.path(), "fake-native", magic);
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    assert!(
        verify_signed(&fake).is_err(),
        "a native image that cannot be verified must still refuse startup"
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = fake; // signing is not enforced on Linux per spec §6.1 row 11
}

/// A disabled entry does not spawn, so it must not be able to refuse startup.
#[test]
fn disabled_servers_are_outside_admission() {
    let dir = TempDir::new().unwrap();
    let bin = write(dir.path(), "drifted-mcp", b"v2 bytes\n");
    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: "weather".into(),
        command: bin.display().to_string(),
        args: vec![],
        // Pin of some other content → rule 6 drift, which normally refuses.
        binary_sha256: Some("0".repeat(64)),
        ..Default::default()
    });
    assert!(
        verify_mcp_supply_chain(&[], &profile).is_err(),
        "sanity: while enabled, the drifted pin refuses startup"
    );

    profile.set_mcp_enabled("weather", false);
    let result = verify_mcp_supply_chain(&[], &profile);
    assert!(
        result.is_ok(),
        "`mur agent mcp disable` must be a working recovery path: {result:?}"
    );
}
