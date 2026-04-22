use mur_common::AgentProfile;
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

fn read_profile(mur_home: &std::path::Path, name: &str) -> AgentProfile {
    let yaml =
        std::fs::read_to_string(mur_home.join("agents").join(name).join("profile.yaml")).unwrap();
    serde_yaml_ng::from_str(&yaml).unwrap()
}

fn run(mur_home: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mur = env!("CARGO_BIN_EXE_mur");
    Command::new(mur)
        .env("MUR_HOME", mur_home)
        .args(args)
        .output()
        .expect("spawn mur")
}

#[test]
fn mcp_add_syncs_profile_and_spawn_allowlist() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let out = run(
        mur_home.path(),
        &[
            "agent",
            "mcp",
            "add",
            "agent_x",
            "srv1",
            "--command",
            "/bin/echo",
            "--arg",
            "hello",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let p = read_profile(mur_home.path(), "agent_x");
    assert_eq!(p.mcp_servers.len(), 1);
    assert_eq!(p.mcp_servers[0].name, "srv1");
    assert_eq!(p.mcp_servers[0].command, "/bin/echo");
    assert_eq!(p.mcp_servers[0].args, vec!["hello".to_string()]);
    assert!(
        p.entitlements
            .processes
            .spawn
            .allowed
            .contains(&"/bin/echo".to_string()),
        "spawn.allowed must include the mcp command: {:?}",
        p.entitlements.processes.spawn.allowed
    );
}

#[test]
fn mcp_list_prints_server_ids() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let _ = run(
        mur_home.path(),
        &[
            "agent",
            "mcp",
            "add",
            "agent_x",
            "srv1",
            "--command",
            "/bin/echo",
        ],
    );

    let out = run(mur_home.path(), &["agent", "mcp", "list", "agent_x"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("srv1"), "list output missing srv1: {body}");
    assert!(body.contains("/bin/echo"));
}

#[test]
fn mcp_remove_and_rename() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let _ = run(
        mur_home.path(),
        &[
            "agent",
            "mcp",
            "add",
            "agent_x",
            "srv1",
            "--command",
            "/bin/echo",
        ],
    );
    let _ = run(
        mur_home.path(),
        &["agent", "mcp", "rename", "agent_x", "srv1", "srv2"],
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert_eq!(p.mcp_servers[0].name, "srv2");

    let out = run(
        mur_home.path(),
        &["agent", "mcp", "remove", "agent_x", "srv2"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(p.mcp_servers.is_empty());
}
