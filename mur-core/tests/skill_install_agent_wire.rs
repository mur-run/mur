//! Wire-level integration test for `mur skill install agent://...`.
//!
//! Boots a real `mur-agent-runtime` for the source agent, dials its
//! Unix socket for `skills/get`, and verifies that the install
//! pipeline applies trust, appends the transfer chain, writes the
//! skill, and registers it on the calling agent's profile.
//!
//! The runtime binary must be present alongside the mur binary in the
//! cargo target dir; if absent, tests are skipped so default cargo test
//! runs stay green.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use mur_common::agent::AgentProfile;
use mur_common::skill::{
    TrustLevel, content_hash_for_trust, global_skill_dir, parse_canonical, read_from_dir,
    write_to_dir,
};
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_install::cmd_install;
use tempfile::TempDir;

fn locate_runtime_binary() -> Option<PathBuf> {
    let mur = std::path::Path::new(env!("CARGO_BIN_EXE_mur"));
    let candidate = mur.parent()?.join("mur-agent-runtime");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

fn write_profile(
    home: &std::path::Path,
    name: &str,
    unix_sock: Option<&str>,
) -> std::path::PathBuf {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut profile = AgentProfile {
        name: name.to_string(),
        ..AgentProfile::default_for_tests()
    };
    profile.transport.stdio = true;
    profile.transport.socket.enabled = unix_sock.is_some();
    if let Some(sock) = unix_sock {
        profile.transport.socket.bind = format!("unix://{sock}");
    }
    let yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

struct RuntimeGuard {
    child: Child,
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let _ = self.child.wait();
    }
}

fn boot_runtime(
    home: &std::path::Path,
    agent: &str,
    runtime_bin: &str,
    sock_path: &str,
) -> RuntimeGuard {
    let child = Command::new(runtime_bin)
        .env("MUR_HOME", home)
        .args(["--profile", agent])
        .spawn()
        .expect("spawn runtime");
    // Two conditions, both waited for rather than assumed:
    //
    //   running.lock non-empty — the runtime came up at all. The macOS SBPL
    //   sandbox may kill it mid-startup, and a zero-byte lock is that signal.
    //
    //   the socket exists — it is actually reachable. This used to be a flat
    //   `sleep(100ms)` after the lock appeared, which is a bet, not a wait: on
    //   a loaded CI runner the bind had not happened yet and the test died
    //   later with `connect …/alice.sock: No such file or directory`, pointing
    //   at the install path instead of at startup. Waiting on the real
    //   indicator also returns as soon as it is ready, so the common case is
    //   faster than the fixed sleep it replaces.
    let lock = home.join("agents").join(agent).join("running.lock");
    let sock = std::path::Path::new(sock_path);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        if let Ok(meta) = std::fs::metadata(&lock)
            && meta.len() > 0
            && sock.exists()
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        found,
        "runtime did not come up within 5s: running.lock non-empty = {}, socket {sock_path} present = {} \
         (sandbox may have killed it mid-startup)",
        std::fs::metadata(&lock)
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        sock.exists()
    );
    RuntimeGuard { child }
}

#[test]
fn wire_install_pulls_via_real_socket() {
    let Some(runtime_bin) = locate_runtime_binary() else {
        eprintln!("(skipping — mur-agent-runtime binary not present in target dir)");
        return;
    };
    let runtime_str = runtime_bin.to_str().unwrap();

    let home = TempDir::new().unwrap();
    let sock_path = home.path().join("alice.sock").to_str().unwrap().to_string();

    // Source agent "alice".
    write_profile(home.path(), "alice", Some(&sock_path));
    let manifest = parse_canonical(
        r#"
name: find-prices
version: 1.0.0
publisher: human:alice
description: Find product prices
category: workflow
content:
  abstract: Searches product prices.
  context: "Full procedure."
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "find-prices"), &manifest).unwrap();

    // Target agent "bob".
    let bob_profile = write_profile(home.path(), "bob", None);

    // Boot alice; her runtime serves `skills/get` on the unix socket.
    let _alice = boot_runtime(home.path(), "alice", runtime_str, &sock_path);

    // Install. SAFETY: env mutation isn't thread-safe across parallel tests.
    // This test file must be run with `--test-threads=1`.
    unsafe { std::env::set_var("MUR_AGENT_NAME", "bob") };
    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", runtime_str) };
    let result = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://alice/find-prices",
    );
    unsafe { std::env::remove_var("MUR_AGENT_NAME") };
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    result.unwrap();

    // 1. Skill file is on disk with transfer_chain appended.
    let installed = read_from_dir(&global_skill_dir(home.path(), "find-prices")).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://alice"]);

    // 2. Trust entry is Sandboxed (no registry cache).
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let key = content_hash_for_trust(&installed).unwrap();
    let entry = trust.lookup(&key).expect("trust entry exists");
    assert!(matches!(entry.level, TrustLevel::Sandboxed));

    // 3. Bob's profile carries the SkillCardEntry.
    let bob_yaml = std::fs::read_to_string(&bob_profile).unwrap();
    let bob: AgentProfile = serde_yaml_ng::from_str(&bob_yaml).unwrap();
    assert_eq!(bob.installed_skills.len(), 1);
    assert_eq!(bob.installed_skills[0].publisher, "human:alice");
}

#[test]
fn wire_install_uses_ephemeral_when_source_offline() {
    let Some(runtime_bin) = locate_runtime_binary() else {
        eprintln!("(skipping — mur-agent-runtime binary not present in target dir)");
        return;
    };
    let runtime_str = runtime_bin.to_str().unwrap();

    let home = TempDir::new().unwrap();

    // Source agent "carol" — profile present but NOT running. The
    // install path must spawn the runtime ephemerally to serve
    // skills/get over stdio.
    write_profile(home.path(), "carol", None);
    let manifest = parse_canonical(
        r#"
name: offline-skill
version: 1.0.0
publisher: human:carol
description: d
category: context
content:
  abstract: a
  context: b
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "offline-skill"), &manifest).unwrap();

    write_profile(home.path(), "dave", None);
    unsafe { std::env::set_var("MUR_AGENT_NAME", "dave") };
    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", runtime_str) };
    let result = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://carol/offline-skill",
    );
    unsafe { std::env::remove_var("MUR_AGENT_NAME") };
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    result.unwrap();

    let installed = read_from_dir(&global_skill_dir(home.path(), "offline-skill")).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://carol"]);
}

#[test]
fn wire_install_propagates_handler_error_for_missing_skill() {
    let Some(runtime_bin) = locate_runtime_binary() else {
        eprintln!("(skipping — mur-agent-runtime binary not present in target dir)");
        return;
    };
    let runtime_str = runtime_bin.to_str().unwrap();

    let home = TempDir::new().unwrap();
    let sock_path = home.path().join("eve.sock").to_str().unwrap().to_string();
    write_profile(home.path(), "eve", Some(&sock_path));
    let _eve = boot_runtime(home.path(), "eve", runtime_str, &sock_path);

    unsafe { std::env::set_var("MUR_AGENT_RUNTIME_BIN", runtime_str) };
    let err = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://eve/no-such-skill",
    )
    .unwrap_err();
    unsafe { std::env::remove_var("MUR_AGENT_RUNTIME_BIN") };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("not found") || msg.contains("internal:"),
        "unexpected error: {msg}"
    );
}
