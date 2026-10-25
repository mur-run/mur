//! One test per [`PinOutcome`] variant, plus the ordering rule that the
//! vendored `package` check runs before the interpreter skip. Boot's mapping
//! of these outcomes is covered by `tests/b0_rule6_mcp_pin.rs`.

use super::{PinOutcome, verify_entry_pin};
use mur_common::AgentProfile;
use mur_common::agent::{McpPackagePin, McpServerEntry};
use sha2::{Digest, Sha256};
use std::path::Path;
use tempfile::TempDir;

fn profile() -> AgentProfile {
    serde_yaml_ng::from_str(include_str!("../../../tests/fixtures/profile_minimal.yaml"))
        .expect("fixture parse")
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn pinned(command: &Path, pin: Option<String>) -> McpServerEntry {
    McpServerEntry {
        name: "probe".into(),
        command: command.display().to_string(),
        binary_sha256: pin,
        ..Default::default()
    }
}

fn vendored(install_dir: &Path, lockfile_sha256: &str) -> McpServerEntry {
    McpServerEntry {
        name: "fetch-mcp".into(),
        // An interpreter: proves the package check wins over the skip.
        command: "node".into(),
        binary_sha256: Some(sha256_hex(b"node")),
        package: Some(McpPackagePin {
            runner: "npm".into(),
            name: "@yawlabs/fetch-mcp".into(),
            version: "0.3.6".into(),
            install_dir: install_dir.display().to_string(),
            lockfile_sha256: lockfile_sha256.into(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn matching_binary_is_verified() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("probe-bin");
    std::fs::write(&bin, b"approved\n").unwrap();
    let entry = pinned(&bin, Some(sha256_hex(b"approved\n")));
    assert_eq!(verify_entry_pin(&entry, &profile()), PinOutcome::Verified);
}

#[test]
fn drifted_binary_is_a_hard_fail_carrying_the_refusal() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("probe-bin");
    std::fs::write(&bin, b"tampered\n").unwrap();
    let entry = pinned(&bin, Some(sha256_hex(b"approved\n")));
    match verify_entry_pin(&entry, &profile()) {
        PinOutcome::HardFail(msg) => assert!(msg.contains("changed since install"), "{msg}"),
        other => panic!("expected HardFail, got {other:?}"),
    }
}

#[test]
fn unresolvable_command_is_a_soft_fail() {
    let dir = TempDir::new().unwrap();
    let entry = pinned(&dir.path().join("gone"), Some(sha256_hex(b"x")));
    assert!(matches!(
        verify_entry_pin(&entry, &profile()),
        PinOutcome::SoftFail(_)
    ));
}

#[test]
fn unpinned_entry_is_skipped() {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("probe-bin");
    std::fs::write(&bin, b"anything\n").unwrap();
    assert_eq!(
        verify_entry_pin(&pinned(&bin, None), &profile()),
        PinOutcome::Skipped
    );
}

#[test]
fn interpreter_entry_is_skipped() {
    let entry = pinned(Path::new("node"), Some(sha256_hex(b"node")));
    assert_eq!(verify_entry_pin(&entry, &profile()), PinOutcome::Skipped);
}

#[test]
fn vendored_package_is_checked_before_the_interpreter_skip() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("package-lock.json"), b"lock v1").unwrap();

    let ok = vendored(dir.path(), &sha256_hex(b"lock v1"));
    assert_eq!(verify_entry_pin(&ok, &profile()), PinOutcome::Verified);

    let drifted = vendored(dir.path(), &sha256_hex(b"lock v0"));
    match verify_entry_pin(&drifted, &profile()) {
        PinOutcome::HardFail(msg) => assert!(msg.contains("no longer matches"), "{msg}"),
        other => panic!("expected HardFail, got {other:?}"),
    }
}

#[test]
fn vendored_package_with_missing_install_is_a_soft_fail() {
    let dir = TempDir::new().unwrap();
    let entry = vendored(&dir.path().join("removed"), &sha256_hex(b"lock v1"));
    assert!(matches!(
        verify_entry_pin(&entry, &profile()),
        PinOutcome::SoftFail(_)
    ));
}
