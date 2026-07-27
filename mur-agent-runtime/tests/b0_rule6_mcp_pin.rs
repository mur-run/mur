//! B0 rule 6 / M9.3 — install-time MCP binary pin refuses startup on drift.
//!
//! Load a minimal `AgentProfile`, inject a synthetic `mcp_servers` entry whose
//! `binary_sha256` either matches or differs from the file on disk, then assert
//! the outcome of [`verify_mcp_supply_chain`].
//!
//! These used to go through `B0SafetyHook::on_startup` and were gated to Linux,
//! because on macOS the rule 11 codesign check fired first and refused the
//! unsigned synthetic binary. Rules 6 and 11 now live in a free function that
//! takes the signature-check list as its own argument (they moved out of the
//! observe-only hook phase, which discarded their errors — #791), so passing an
//! empty list exercises rule 6 in isolation on every platform.

use mur_agent_runtime::hooks::b0::verify_mcp_supply_chain;
use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tempfile::TempDir;

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

fn profile_with_pin(command: &std::path::Path, name: &str, pin: Option<String>) -> AgentProfile {
    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: name.into(),
        command: command.display().to_string(),
        args: vec![],
        binary_sha256: pin,
        ..Default::default()
    });
    profile
}

#[test]
fn matching_binary_hash_passes_startup() {
    let dir = TempDir::new().unwrap();
    let payload = b"pretend MCP body v1\n";
    let bin = write_fake_binary(dir.path(), payload);
    let profile = profile_with_pin(&bin, "weather", Some(sha256_hex(payload)));

    let result = verify_mcp_supply_chain(&[], &profile);
    assert!(result.is_ok(), "matching pin should pass: {result:?}");
}

#[test]
fn binary_drift_fails_startup_with_inspect_hint() {
    let dir = TempDir::new().unwrap();
    let installed = b"pretend MCP body v1\n";
    let bin = write_fake_binary(dir.path(), installed);
    // Pin records v1's hash; rewrite the file to v2's bytes.
    let pinned_hash = sha256_hex(installed);
    std::fs::write(&bin, b"pretend MCP body v2 -- drifted\n").unwrap();

    let profile = profile_with_pin(&bin, "weather", Some(pinned_hash));

    let msg = verify_mcp_supply_chain(&[], &profile).expect_err("drift should fail startup");
    assert!(msg.contains("B0 rule 6"), "got {msg}");
    assert!(msg.contains("mur agent mcp inspect weather"), "got {msg}");
    assert!(msg.contains("mur agent mcp pin weather"), "got {msg}");
}

#[test]
fn missing_binary_soft_fails_does_not_block_startup() {
    let dir = TempDir::new().unwrap();
    // Path that doesn't exist — the supervisor would catch this in its own
    // resolve step in production, but the verify path must not crash on a
    // missing file (user uninstalled the MCP without removing it from
    // profile.yaml).
    let phantom = dir.path().join("phantom-mcp");
    let profile = profile_with_pin(&phantom, "ghost", Some("deadbeef".repeat(8)));

    let result = verify_mcp_supply_chain(&[], &profile);
    assert!(
        result.is_ok(),
        "missing binary should soft-fail (warn, allow start): {result:?}",
    );
}

#[test]
fn pre_m9_entry_without_pin_skipped_cleanly() {
    let dir = TempDir::new().unwrap();
    let bin = write_fake_binary(dir.path(), b"any bytes");
    // binary_sha256 stays None — pre-M9 entry, exempt from rule 6.
    let profile = profile_with_pin(&bin, "legacy", None);

    let result = verify_mcp_supply_chain(&[], &profile);
    assert!(
        result.is_ok(),
        "pre-M9 (no pin) entry should pass: {result:?}"
    );
}

// ── Vendored packages (#796) ────────────────────────────────────────────────
//
// A vendored entry launches `node <install>/…`, which is interpreter-shaped —
// but MUR owns that directory, so its lockfile IS verifiable and must be
// enforced rather than waved through by the interpreter exemption.

fn profile_with_vendored(install_dir: &std::path::Path, lockfile_sha256: &str) -> AgentProfile {
    let mut profile = minimal_profile();
    profile.mcp_servers.push(McpServerEntry {
        name: "fetch-mcp".into(),
        command: "node".into(),
        args: vec![
            install_dir
                .join("node_modules/x/dist/index.js")
                .display()
                .to_string(),
        ],
        // No binary pin: hashing `node` was never the point.
        binary_sha256: None,
        package: Some(mur_common::agent::McpPackagePin {
            runner: "npm".into(),
            name: "@yawlabs/fetch-mcp".into(),
            version: "0.3.6".into(),
            install_dir: install_dir.display().to_string(),
            lockfile_sha256: lockfile_sha256.into(),
            // Functional update so a new pin field doesn't break this fixture,
            // the way adding `signatures_missing` did.
            ..Default::default()
        }),
        ..Default::default()
    });
    profile
}

fn write_lockfile(dir: &std::path::Path, body: &[u8]) -> String {
    std::fs::write(dir.join("package-lock.json"), body).unwrap();
    sha256_hex(body)
}

#[test]
fn vendored_package_passes_when_the_lockfile_matches() {
    let dir = TempDir::new().unwrap();
    let sha = write_lockfile(dir.path(), b"{\"lockfileVersion\":3}");
    let result = verify_mcp_supply_chain(&[], &profile_with_vendored(dir.path(), &sha));
    assert!(result.is_ok(), "matching lockfile should pass: {result:?}");
}

#[test]
fn vendored_package_refuses_startup_when_the_tree_changed() {
    let dir = TempDir::new().unwrap();
    let sha = write_lockfile(dir.path(), b"{\"lockfileVersion\":3}");
    // A dependency swapped underneath the install rewrites the lockfile.
    std::fs::write(
        dir.path().join("package-lock.json"),
        b"{\"lockfileVersion\":3,\"packages\":{\"node_modules/evil\":{}}}",
    )
    .unwrap();

    let msg = verify_mcp_supply_chain(&[], &profile_with_vendored(dir.path(), &sha))
        .expect_err("a changed vendored tree must refuse startup");
    assert!(msg.contains("vendored MCP `fetch-mcp`"), "got {msg}");
    assert!(msg.contains("@yawlabs/fetch-mcp@0.3.6"), "got {msg}");
    assert!(msg.contains("mur agent mcp vendor"), "got {msg}");
}

/// Deleting the install must not lock the user out of their own agent: the
/// recovery command lives in the CLI, which they can't reach if the agent
/// refuses to boot over a directory they removed.
#[test]
fn vendored_package_with_a_missing_install_does_not_block_startup() {
    let dir = TempDir::new().unwrap();
    let profile = profile_with_vendored(dir.path(), &"a".repeat(64)); // no lockfile written
    let result = verify_mcp_supply_chain(&[], &profile);
    assert!(
        result.is_ok(),
        "a missing install should warn, not strand the agent: {result:?}"
    );
}
