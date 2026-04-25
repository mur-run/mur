// Windows: gated — depends on `mur agent create` (unix symlink).
#![cfg(unix)]

use mur_common::AgentProfile;
use mur_common::agent::{NetworkOutboundMode, SpawnMode};
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
fn perm_set_mode_changes_outbound_mode() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let out = run(
        mur_home.path(),
        &[
            "agent",
            "perm",
            "set-mode",
            "agent_x",
            "network.outbound",
            "unrestricted",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert_eq!(
        p.entitlements.network.outbound.mode,
        NetworkOutboundMode::Unrestricted
    );
}

#[test]
fn perm_allow_host_appends_and_list_prints() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let _ = run(
        mur_home.path(),
        &["agent", "perm", "allow-host", "agent_x", "*.example.com"],
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(
        p.entitlements
            .network
            .outbound
            .allow_hosts
            .contains(&"*.example.com".to_string())
    );
    let list_out = run(mur_home.path(), &["agent", "perm", "list-hosts", "agent_x"]);
    assert!(list_out.status.success());
    let body = String::from_utf8(list_out.stdout).unwrap();
    assert!(body.contains("*.example.com"));
}

#[test]
fn perm_filesystem_and_spawn_mutators() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let _ = run(
        mur_home.path(),
        &["agent", "perm", "allow-read", "agent_x", "/var/log"],
    );
    let _ = run(
        mur_home.path(),
        &["agent", "perm", "allow-write", "agent_x", "/tmp/out"],
    );
    let _ = run(
        mur_home.path(),
        &["agent", "perm", "deny-path", "agent_x", "~/.config/secret"],
    );
    let _ = run(
        mur_home.path(),
        &["agent", "perm", "allow-spawn", "agent_x", "/usr/bin/curl"],
    );

    let p = read_profile(mur_home.path(), "agent_x");
    assert!(
        p.entitlements
            .filesystem
            .read
            .contains(&"/var/log".to_string())
    );
    assert!(
        p.entitlements
            .filesystem
            .write
            .contains(&"/tmp/out".to_string())
    );
    assert!(
        p.entitlements
            .filesystem
            .deny
            .contains(&"~/.config/secret".to_string())
    );
    assert!(matches!(
        p.entitlements.processes.spawn.mode,
        SpawnMode::Allowlist
    ));
    assert!(
        p.entitlements
            .processes
            .spawn
            .allowed
            .contains(&"/usr/bin/curl".to_string())
    );

    // Now deny-spawn removes it.
    let _ = run(
        mur_home.path(),
        &["agent", "perm", "deny-spawn", "agent_x", "/usr/bin/curl"],
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(
        !p.entitlements
            .processes
            .spawn
            .allowed
            .contains(&"/usr/bin/curl".to_string())
    );
}

#[test]
fn perm_set_limit_updates_numeric_field() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let out = run(
        mur_home.path(),
        &["agent", "perm", "set-limit", "agent_x", "memory_mb", "4096"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert_eq!(p.entitlements.limits.memory_mb, 4096);
}

#[test]
fn perm_show_prints_entitlements() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let out = run(mur_home.path(), &["agent", "perm", "show", "agent_x"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(
        body.contains("network"),
        "show should print network section: {body}"
    );
    assert!(body.contains("filesystem"));
}
