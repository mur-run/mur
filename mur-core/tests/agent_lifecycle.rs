// Windows: gated — all tests in this file go through mur_create(), which
// invokes `mur agent create` that creates a unix-style symlink to the
// runtime binary. On Windows the CLI falls back to fs::copy(/tmp/runtime-stub,
// ...) which fails because the source path does not exist. The earlier
// per-test #[cfg(unix)] on stop_sigterms_the_recorded_pid and
// rename_refuses_when_agent_is_running was incomplete — the remove/rename
// tests also rely on mur_create's unix path.
#![cfg(unix)]

use mur_common::{AgentProfile, LockFile, agent::LockTransports};
use std::process::Command;
use tempfile::TempDir;

fn mur_create(mur_home: &std::path::Path, bin_dir: &std::path::Path, name: &str) {
    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(["agent", "create", name, "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_lock_with_pid(mur_home: &std::path::Path, name: &str, pid: u32) {
    let path = mur_home.join("agents").join(name).join("running.lock");
    let lock = LockFile {
        schema: 1,
        uuid: "0192f5a1-28ab-7111-8000-0000000000aa".into(),
        name: name.into(),
        pid,
        ppid: 1,
        started_at: chrono::Utc::now().to_rfc3339(),
        binary_version: "test".into(),
        transports: LockTransports {
            stdio: true,
            unix_socket: None,
            tcp: None,
            webhook: None,
        },
        card_digest: "sha256:x".into(),
        capabilities: vec![],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
    };
    std::fs::write(path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[test]
#[cfg(unix)]
fn stop_sigterms_the_recorded_pid() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    // Spawn a long sleep and write its pid into running.lock.
    let mut child = Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    assert!(is_alive(pid));
    write_lock_with_pid(mur_home.path(), "agent_x", pid);

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "stop", "agent_x"])
        .output()
        .expect("spawn mur stop");
    assert!(
        out.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The sleep process must be gone now (SIGTERM handled, exit) and the lock removed.
    let _ = child.wait();
    assert!(!is_alive(pid), "sleep should have exited after SIGTERM");
    let lock_path = mur_home.path().join("agents/agent_x/running.lock");
    assert!(!lock_path.exists(), "running.lock should be removed");
}

#[test]
fn remove_deletes_symlink_but_keeps_dir_without_purge() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let dir = mur_home.path().join("agents/agent_x");
    let symlink = bin_dir.path().join("mur_agent_agent_x");
    assert!(dir.exists() && symlink.symlink_metadata().is_ok());

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "remove", "agent_x"])
        .output()
        .expect("spawn mur remove");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        symlink.symlink_metadata().is_err(),
        "symlink should be removed"
    );
    assert!(dir.exists(), "dir should be preserved without --purge");
}

#[test]
fn remove_purge_deletes_dir() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let dir = mur_home.path().join("agents/agent_x");

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "remove", "agent_x", "--purge"])
        .output()
        .expect("spawn mur remove --purge");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!dir.exists(), "dir should be purged");
}

#[test]
fn rename_updates_dir_profile_name_and_symlink() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_a");

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "rename", "agent_a", "agent_b"])
        .output()
        .expect("spawn mur rename");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let old_dir = mur_home.path().join("agents/agent_a");
    let new_dir = mur_home.path().join("agents/agent_b");
    assert!(!old_dir.exists());
    assert!(new_dir.exists());

    let yaml = std::fs::read_to_string(new_dir.join("profile.yaml")).unwrap();
    let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(profile.name, "agent_b");

    let old_sym = bin_dir.path().join("mur_agent_agent_a");
    let new_sym = bin_dir.path().join("mur_agent_agent_b");
    assert!(old_sym.symlink_metadata().is_err());
    assert!(new_sym.symlink_metadata().is_ok());
}

#[test]
#[cfg(unix)]
fn rename_refuses_when_agent_is_running() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_a");
    // Our own pid — always alive.
    write_lock_with_pid(mur_home.path(), "agent_a", std::process::id());

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "rename", "agent_a", "agent_b"])
        .output()
        .expect("spawn mur rename");
    assert!(!out.status.success(), "rename should fail when running");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("running"),
        "stderr should mention running, got: {err}"
    );
}
