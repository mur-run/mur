//! End-to-end proof that allowlisting a binary under `<home>/<dir>/bin`
//! (e.g. `~/.local/bin/mur`) does NOT grant exec over the whole
//! `<home>/<dir>` tree. Before the fix the derived prefix was `~/.local`,
//! which made every interpreter under `~/.local/share` (uv-managed Pythons,
//! pipx venvs) executable from inside the seal.
//!
//! The policy half runs everywhere. The kernel half needs `sandbox-exec` to
//! apply a profile, which macOS refuses from inside an already-sandboxed
//! process, so it is gated behind `MUR_TEST_SANDBOX=1` exactly like
//! `b1_spawn_allowlist_enforce.rs`.
//!
//! This file holds a single test on purpose: `SandboxPolicy::from_entitlements`
//! reads the home directory from `$HOME`, and the test points `$HOME` at a
//! temp tree. One test per binary keeps that env change from racing others.
#![cfg(target_os = "macos")]

use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_agent_runtime::sandbox::macos::build_sbpl_profile;
use mur_common::agent::{
    Entitlements, FilesystemEntitlement, InboundNetwork, NetworkEntitlement, NetworkOutboundMode,
    OutboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Copy `/usr/bin/true` to `dest` (creating parents) and mark it executable,
/// so it is a real binary outside every system exec path.
fn plant_executable(dest: &Path) {
    std::fs::create_dir_all(dest.parent().expect("dest has a parent")).expect("mkdir -p");
    std::fs::copy("/usr/bin/true", dest).expect("copy /usr/bin/true");
    let mut perms = std::fs::metadata(dest).expect("stat").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(dest, perms).expect("chmod");
}

fn allowlist_entitlements(allowed: &Path) -> Entitlements {
    Entitlements {
        network: NetworkEntitlement {
            inbound: InboundNetwork { ports: vec![] },
            outbound: OutboundNetwork {
                mode: NetworkOutboundMode::Unrestricted,
                allow_hosts: vec![],
                allow_ports: vec![],
                protocols: vec!["tcp".to_string()],
                resolve_dns: Default::default(),
            },
        },
        filesystem: FilesystemEntitlement {
            read: vec![],
            write: vec![],
            deny: vec![],
        },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement {
                mode: SpawnMode::Allowlist,
                allowed: vec![allowed.to_string_lossy().into_owned()],
                allowed_dirs: vec![],
            },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
        tools: vec![],
        fail_closed_on_sandbox_error: true,
    }
}

fn run_under_profile(profile: &Path, binary: &Path) -> std::process::ExitStatus {
    Command::new("sandbox-exec")
        .arg("-f")
        .arg(profile)
        .arg(binary)
        .status()
        .expect("failed to spawn sandbox-exec itself")
}

#[test]
fn home_child_bin_grant_does_not_expose_sibling_share_interpreters() {
    // Canonical temp root: on macOS the temp dir often sits behind a
    // symlink (`/var` -> `/private/var`), and the prefix guard compares
    // canonical paths.
    let tmp = tempfile::TempDir::new().expect("temp home");
    let home: PathBuf = std::fs::canonicalize(tmp.path()).expect("canonical temp home");

    let tool = home.join(".local/bin/tool");
    let python = home.join(".local/share/uv/python/cpython-3.12/bin/python3");
    plant_executable(&tool);
    plant_executable(&python);

    // Serialized and restored on unwind by the guard.
    let _env = mur_common::test_env::EnvGuard::set([("HOME", &home)]);

    let agent_home = home.join(".mur/agents/probe");
    std::fs::create_dir_all(&agent_home).expect("agent home");
    let policy = SandboxPolicy::from_entitlements(&allowlist_entitlements(&tool), &agent_home);

    // Policy half: the derived prefix stops at `bin/`.
    assert!(
        policy
            .spawn_allowed_prefixes
            .contains(&home.join(".local/bin")),
        "expected `<home>/.local/bin` as the derived prefix, got {:?}",
        policy.spawn_allowed_prefixes
    );
    assert!(
        !policy.spawn_allowed_prefixes.contains(&home.join(".local")),
        "`<home>/.local` must never be a derived prefix, got {:?}",
        policy.spawn_allowed_prefixes
    );

    // Kernel half.
    if std::env::var("MUR_TEST_SANDBOX").as_deref() != Ok("1") {
        eprintln!(
            "skipping kernel half: set MUR_TEST_SANDBOX=1 in a shell that is not \
             itself sandboxed (macOS forbids nesting sandbox-exec)"
        );
        return;
    }
    let profile = home.join("probe.sb");
    std::fs::write(&profile, build_sbpl_profile(&policy)).expect("write profile");

    let allowed = run_under_profile(&profile, &tool);
    assert!(
        allowed.success(),
        "the allowlisted `<home>/.local/bin/tool` must still exec, got {allowed:?}"
    );
    let denied = run_under_profile(&profile, &python);
    assert!(
        !denied.success(),
        "a uv-style interpreter under `<home>/.local/share` must be denied by \
         the kernel, got {denied:?}"
    );
}
