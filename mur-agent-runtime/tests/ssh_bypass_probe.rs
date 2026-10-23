//! The ssh-agent bypass probe against the real runtime binary and the real
//! SBPL profile (plan Phase 1, Task 1.1 and the spec's macOS integration test).
//!
//! The probe child installs a profile on itself, which macOS refuses inside
//! an already-sandboxed process, so every test skips loudly from a sealed
//! shell (#1697) and fails under CI, where runners are never sealed.
#![cfg(target_os = "macos")]

use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::macos::build_sbpl_profile;
use mur_agent_runtime::sandbox::ssh_bypass::probe::{
    EXIT_CONNECTED, PROBE_SUBCOMMAND, PROBE_TIMEOUT, ProbeOutcome, run_probe,
};
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_mur-agent-runtime");

/// Set to a non-empty value by CI providers (GitHub Actions, GitLab,
/// Buildkite, ...). Same convention as `sandbox_build_lane.rs`.
const CI_ENV_VAR: &str = "CI";

fn in_ci() -> bool {
    std::env::var_os(CI_ENV_VAR).is_some_and(|v| !v.is_empty() && v != "false" && v != "0")
}

/// Skip from a sealed local shell, but fail under CI: CI runners are not
/// sandboxed, and nextest's ci profile drops a passing test's stderr, so a
/// silent skip there would be a green check that measured nothing.
fn sealed(test: &str) -> bool {
    let sealed = mur_agent_runtime::sandbox::current_process_sealed();
    if sealed {
        assert!(
            !in_ci(),
            "{test}: process is sandboxed under CI, so the probe cannot run; \
             CI runners are expected to be unsealed (#1697)"
        );
        eprintln!(
            "SKIP {test}: this process is already sandboxed, so the probe child \
             cannot install a profile (#1697)"
        );
    }
    sealed
}

/// A listening socket in its own canonical tempdir (the kernel matches the
/// resolved path, so the allow rule must name the resolved dir too).
fn listener() -> (tempfile::TempDir, PathBuf, UnixListener) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let sock = root.join("a.sock");
    let l = UnixListener::bind(&sock).unwrap();
    (dir, sock, l)
}

/// Arbitrary loopback port: its only job is to keep the profile out of the
/// "true Off" arm, which emits no unix-socket allow rules at all. Without it
/// every dial is denied and neither test below measures the carve-out.
const LOOPBACK_PORT: u16 = 1;

fn restricted(scratch: Option<&Path>) -> SandboxPolicy {
    SandboxPolicy {
        net_allow_ports: Some(vec![]),
        net_allow_loopback_ports: vec![LOOPBACK_PORT],
        scratch_dir: scratch.map(Path::to_path_buf),
        ..Default::default()
    }
}

/// Guard the setup itself: the profile must carry the scoped unix-socket
/// carve-out, or a `Denied` proves nothing about the allowlist.
fn assert_has_unix_carve_out(profile: &str) {
    assert!(
        profile.contains("(remote unix-socket (subpath "),
        "profile has no unix-socket subpath allow; the test is not measuring the carve-out:\n{profile}"
    );
}

#[test]
fn the_subcommand_connects_under_a_permissive_profile() {
    if sealed("the_subcommand_connects_under_a_permissive_profile") {
        return;
    }
    let (_d, sock, _l) = listener();
    let mut child = Command::new(BIN)
        .arg(PROBE_SUBCOMMAND)
        .arg(&sock)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"(version 1)(allow default)")
        .unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(EXIT_CONNECTED));
}

#[test]
fn an_upstream_outside_every_allow_rule_is_denied() {
    if sealed("an_upstream_outside_every_allow_rule_is_denied") {
        return;
    }
    let (_d, sock, _l) = listener();
    let profile = build_sbpl_profile(&restricted(None));
    assert_has_unix_carve_out(&profile);
    assert_eq!(
        run_probe(Path::new(BIN), &profile, &sock, PROBE_TIMEOUT),
        ProbeOutcome::Denied
    );
}

#[test]
fn an_upstream_under_an_allowed_root_is_reachable() {
    if sealed("an_upstream_under_an_allowed_root_is_reachable") {
        return;
    }
    let (_d, sock, _l) = listener();
    let profile = build_sbpl_profile(&restricted(sock.parent()));
    assert!(
        profile.contains(&format!(
            "(remote unix-socket (subpath \"{}\"))",
            sock.parent().unwrap().display()
        )),
        "profile does not allow the scratch dir the socket lives in:\n{profile}"
    );
    assert_eq!(
        run_probe(Path::new(BIN), &profile, &sock, PROBE_TIMEOUT),
        ProbeOutcome::Connected
    );
}
