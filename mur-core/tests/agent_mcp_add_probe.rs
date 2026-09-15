//! `mur agent mcp add` proves the server starts before it writes the entry.
//!
//! Before this, the install checked provenance only — the binary resolves, its
//! bytes hash — and never spawned the thing. An entry that could not start was
//! written as a success, and the user met it at the next restart as an agent
//! that would not boot. That is the shape of the 2026-09-15 field report.
#![cfg(unix)]

use std::process::Command;
use tempfile::TempDir;

fn mur() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mur"))
}

fn create_agent(mur_home: &std::path::Path, bin_dir: &std::path::Path, name: &str) {
    let out = mur()
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

fn profile_of(mur_home: &std::path::Path, name: &str) -> String {
    std::fs::read_to_string(mur_home.join("agents").join(name).join("profile.yaml")).unwrap()
}

/// `/bin/echo` resolves and hashes perfectly well, and is not an MCP server.
/// That combination is exactly what used to install cleanly.
#[test]
fn a_server_that_cannot_start_is_not_installed() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    create_agent(mur_home.path(), bin_dir.path(), "agent_x");

    let out = mur()
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_MCP_PROBE_TIMEOUT_S", "3")
        .args([
            "agent",
            "mcp",
            "add",
            "agent_x",
            "notreally",
            "--command",
            "/bin/echo",
            "--force",
        ])
        .output()
        .expect("spawn mur mcp add");

    assert!(
        !out.status.success(),
        "a dead server must not install clean"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did not start"), "got {stderr}");
    assert!(
        stderr.contains("--no-probe"),
        "the way out must be named: {stderr}"
    );
    assert!(
        !profile_of(mur_home.path(), "agent_x").contains("notreally"),
        "a failed probe must leave NO entry behind — that is the whole point"
    );
}

/// The escape hatch still installs, and says what it did not check.
#[test]
fn no_probe_installs_unchecked() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    create_agent(mur_home.path(), bin_dir.path(), "agent_x");

    let out = mur()
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args([
            "agent",
            "mcp",
            "add",
            "agent_x",
            "notreally",
            "--command",
            "/bin/echo",
            "--force",
            "--no-probe",
        ])
        .output()
        .expect("spawn mur mcp add");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        profile_of(mur_home.path(), "agent_x").contains("notreally"),
        "--no-probe must still install"
    );
}

/// An entry added before its binary exists cannot be probed, and that workflow
/// predates this change — it keeps working, with the pin warning it always had.
#[test]
fn an_unresolvable_command_still_installs_as_before() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    create_agent(mur_home.path(), bin_dir.path(), "agent_x");

    let out = mur()
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args([
            "agent",
            "mcp",
            "add",
            "agent_x",
            "later",
            "--command",
            "definitely-not-installed-yet",
            "--force",
        ])
        .output()
        .expect("spawn mur mcp add");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(profile_of(mur_home.path(), "agent_x").contains("later"));
}
