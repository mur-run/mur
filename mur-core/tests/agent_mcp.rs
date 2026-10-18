// Windows: gated — depends on `mur agent create` (unix symlink).
#![cfg(unix)]

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
            // B0 rule 6 / M9.2 — install confirm prompt blocks the
            // (non-TTY) test runner without --force.
            "--force",
            // Bookkeeping fixtures: /bin/echo is not an MCP server and never
            // was. These assert profile sync, the spawn allowlist, list,
            // rename and remove — not whether the server starts, which is
            // `agent_mcp_add_probe.rs`.
            "--no-probe",
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
            "--force",
            // Bookkeeping fixtures: /bin/echo is not an MCP server and never
            // was. These assert profile sync, the spawn allowlist, list,
            // rename and remove — not whether the server starts, which is
            // `agent_mcp_add_probe.rs`.
            "--no-probe",
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
            "--force",
            // Bookkeeping fixtures: /bin/echo is not an MCP server and never
            // was. These assert profile sync, the spawn allowlist, list,
            // rename and remove — not whether the server starts, which is
            // `agent_mcp_add_probe.rs`.
            "--no-probe",
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

/// #1984 item 7: the runtime launches `mur-mcp-server` from MUR's bundled copy
/// (`resolve_mcp_launch_in`), not from PATH. `mcp add` used to pin and grant
/// the PATH copy, so the probe passed and the restarted agent got EPERM on a
/// binary nobody had granted — 0 tools. Install time must judge the same file.
#[test]
fn mcp_add_pins_and_grants_the_bundled_server_the_runtime_launches() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let write_exe = |p: &std::path::Path, body: &[u8]| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let bundled = mur_home.path().join("mcp-servers").join("mur-mcp-server");
    write_exe(&bundled, b"#!/bin/sh\n# bundled copy\n");
    let path_dir = TempDir::new().unwrap();
    write_exe(
        &path_dir.path().join("mur-mcp-server"),
        b"#!/bin/sh\n# PATH copy\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", mur_home.path())
        .env("PATH", path_dir.path())
        .args([
            "agent",
            "mcp",
            "add",
            "agent_x",
            "mur-tools",
            "--command",
            "mur-mcp-server",
            "--force",
            "--no-probe",
        ])
        .output()
        .expect("spawn mur");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let p = read_profile(mur_home.path(), "agent_x");
    let want = hex::encode(Sha256::digest(std::fs::read(&bundled).unwrap()));
    assert_eq!(
        p.mcp_servers[0].binary_sha256.as_deref(),
        Some(want.as_str()),
        "pin must be the bundled copy's hash, not the PATH copy's"
    );
    let allowed = &p.entitlements.processes.spawn.allowed;
    assert!(
        allowed.iter().any(|a| std::path::Path::new(a) == bundled),
        "spawn.allowed must grant the bundled path the runtime execs: {allowed:?}"
    );
}
