// Windows: gated — depends on `mur agent create` (unix symlink) +
// running.lock file semantics including unix sockets.
#![cfg(unix)]

use mur_common::{LockFile, agent::LockTransports};
use std::process::Command;
use tempfile::TempDir;

fn create_agent(mur_home: &std::path::Path, bin_dir: &std::path::Path, name: &str) {
    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(["agent", "create", name, "--no-interactive"])
        .output()
        .expect("spawn mur");
    assert!(
        out.status.success(),
        "create {name} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_running_lock(mur_home: &std::path::Path, name: &str, pid: u32) {
    let agent_home = mur_home.join("agents").join(name);
    let lock = LockFile {
        schema: 1,
        uuid: "0192f5a1-28ab-7111-8000-000000000099".into(),
        name: name.into(),
        pid,
        ppid: 1,
        started_at: chrono::Utc::now().to_rfc3339(),
        binary_version: "mur-agent-runtime test".into(),
        transports: LockTransports {
            stdio: true,
            unix_socket: None,
            tcp: None,
            webhook: None,
        },
        card_digest: "sha256:test".into(),
        capabilities: vec!["a2a.message.send".into()],
        build_sha: String::new(),
        proto_version: 0,
        sandbox: None,
    };
    let bytes = serde_json::to_vec_pretty(&lock).unwrap();
    std::fs::write(agent_home.join("running.lock"), bytes).unwrap();
}

#[test]
fn agent_list_json_classifies_running_vs_stopped() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();

    create_agent(mur_home.path(), bin_dir.path(), "agent_a");
    create_agent(mur_home.path(), bin_dir.path(), "agent_b");

    // Simulate agent_b running by writing a lock with our own pid (always alive).
    write_running_lock(mur_home.path(), "agent_b", std::process::id());

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args(["agent", "list", "--json"])
        .output()
        .expect("spawn mur");
    assert!(
        out.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).expect("utf8");
    let arr: serde_json::Value = serde_json::from_str(body.trim()).expect("json array");
    let arr = arr.as_array().expect("array");
    assert_eq!(arr.len(), 2, "expected 2 agents, got {arr:?}");
    let running: Vec<_> = arr.iter().filter(|a| a["status"] == "running").collect();
    assert_eq!(running.len(), 1, "expected exactly 1 running, got {arr:?}");
    assert_eq!(running[0]["name"], "agent_b");
}

#[test]
fn agent_status_prints_name_and_category() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();

    create_agent(mur_home.path(), bin_dir.path(), "agent_a");
    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args(["agent", "status", "agent_a"])
        .output()
        .expect("spawn mur");
    assert!(
        out.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("agent_a"), "status missing name: {body}");
    assert!(body.contains("custom"), "status missing category: {body}");
}
