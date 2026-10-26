//! The reload gate: a changed `command` is served only on a verified pin, a
//! signed binary and an exec the seal allows; anything else keeps the entry
//! already being served, and says why.

use super::{Refusals, gate_entries};
use mur_common::AgentProfile;
use mur_common::agent::McpServerEntry;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn profile() -> AgentProfile {
    serde_yaml_ng::from_str(include_str!("../tests/fixtures/profile_minimal.yaml"))
        .expect("fixture parse")
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A file standing in for a server binary (not a native image, so the
/// signature check has nothing to judge).
fn bin(dir: &TempDir, name: &str, body: &[u8]) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, body).unwrap();
    p
}

fn entry(command: &Path, pin: Option<String>) -> McpServerEntry {
    McpServerEntry {
        name: "probe".into(),
        command: command.display().to_string(),
        binary_sha256: pin,
        ..Default::default()
    }
}

fn allow_all(_: &Path) -> bool {
    true
}

fn gate(
    served: &McpServerEntry,
    candidate: &McpServerEntry,
    permitted: &dyn Fn(&Path) -> bool,
) -> (McpServerEntry, Refusals) {
    let (mut out, refusals) = gate_entries(
        &profile(),
        std::slice::from_ref(served),
        vec![candidate.clone()],
        permitted,
    );
    (out.remove(0), refusals)
}

#[test]
fn a_pinned_command_change_that_verifies_is_served() {
    let dir = TempDir::new().unwrap();
    let old = entry(&bin(&dir, "a", b"a"), Some(sha(b"a")));
    let new = entry(&bin(&dir, "b", b"b"), Some(sha(b"b")));
    let (served, refusals) = gate(&old, &new, &allow_all);
    assert_eq!(served, new);
    assert!(refusals.is_empty(), "{refusals:?}");
}

#[test]
fn a_drifted_binary_keeps_the_served_entry_and_records_why() {
    let dir = TempDir::new().unwrap();
    let old = entry(&bin(&dir, "a", b"a"), Some(sha(b"a")));
    let new = entry(&bin(&dir, "b", b"tampered"), Some(sha(b"b")));
    let (served, refusals) = gate(&old, &new, &allow_all);
    assert_eq!(served, old);
    assert!(
        refusals["probe"].contains("changed since install"),
        "{refusals:?}"
    );
}

#[test]
fn a_command_change_without_a_pin_is_refused() {
    let dir = TempDir::new().unwrap();
    let old = entry(&bin(&dir, "a", b"a"), None);
    let new = entry(&bin(&dir, "b", b"b"), None);
    let (served, refusals) = gate(&old, &new, &allow_all);
    assert_eq!(served, old);
    assert!(refusals["probe"].contains("no pin"), "{refusals:?}");
}

#[test]
fn a_missing_binary_is_refused_although_boot_would_only_warn() {
    let dir = TempDir::new().unwrap();
    let old = entry(&bin(&dir, "a", b"a"), Some(sha(b"a")));
    let new = entry(&dir.path().join("gone"), Some(sha(b"b")));
    let (served, refusals) = gate(&old, &new, &allow_all);
    assert_eq!(served, old);
    assert!(
        refusals["probe"].contains("could not be checked"),
        "{refusals:?}"
    );
}

#[test]
fn an_exec_the_seal_refuses_is_refused() {
    let dir = TempDir::new().unwrap();
    let old = entry(&bin(&dir, "a", b"a"), Some(sha(b"a")));
    let new = entry(&bin(&dir, "b", b"b"), Some(sha(b"b")));
    let (served, refusals) = gate(&old, &new, &|_: &Path| false);
    assert_eq!(served, old);
    assert!(
        refusals["probe"].contains("does not allow exec"),
        "{refusals:?}"
    );
}

#[test]
fn an_unchanged_command_is_not_rechecked() {
    // Args and timeout edits are hot by classification alone; the gate must
    // not refuse them over a binary it was never asked about.
    let dir = TempDir::new().unwrap();
    let old = entry(&dir.path().join("not-installed-yet"), None);
    let mut new = old.clone();
    new.args = vec!["--verbose".into()];
    let (served, refusals) = gate(&old, &new, &|_: &Path| false);
    assert_eq!(served, new);
    assert!(refusals.is_empty());
}

#[test]
fn a_refusal_falls_back_to_what_was_served_not_to_boot() {
    // Served moved to B by an earlier good reload; a bad edit to C must keep
    // B, not restart the server onto the boot binary A.
    let dir = TempDir::new().unwrap();
    let b = entry(&bin(&dir, "b", b"b"), Some(sha(b"b")));
    let c = entry(&bin(&dir, "c", b"tampered"), Some(sha(b"c")));
    let (served, _) = gate(&b, &c, &allow_all);
    assert_eq!(served, b);
}
