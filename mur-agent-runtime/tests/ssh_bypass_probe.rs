//! The ssh-agent bypass probe against the real runtime binary and the real
//! SBPL profile (plan Phase 1, Task 1.1 and the spec's macOS integration test).
//!
//! The probe child installs a profile on itself, which macOS refuses inside
//! an already-sandboxed process, so every test skips loudly from a sealed
//! shell (#1697).
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

fn sealed(test: &str) -> bool {
    let sealed = mur_agent_runtime::sandbox::current_process_sealed();
    if sealed {
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

fn restricted(scratch: Option<&Path>) -> SandboxPolicy {
    SandboxPolicy {
        net_allow_ports: Some(vec![]),
        scratch_dir: scratch.map(Path::to_path_buf),
        ..Default::default()
    }
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
    assert_eq!(
        run_probe(Path::new(BIN), &profile, &sock, PROBE_TIMEOUT),
        ProbeOutcome::Connected
    );
}
