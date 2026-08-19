//! Track C1 / M-c1.6: `mur agent companion connector add --platform stub`
//! integration tests.
//!
//! Windows: gated — the scaffold writes Unix-style symlinks indirectly via
//! identity files; gating matches `agent_create.rs`.
#![cfg(unix)]

use std::process::Command;
use tempfile::TempDir;

#[test]
fn stub_bridge_creates_expected_layout() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();

    let exe = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(exe)
        .args([
            "agent",
            "companion",
            "connector",
            "add",
            "stub_bridge",
            "--platform",
            "stub",
            "--default-route",
            "coach",
        ])
        .env("MUR_HOME", &mur_home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dir = mur_home.join("agents/stub_bridge");
    assert!(dir.join("profile.yaml").exists(), "profile.yaml missing");
    assert!(dir.join("routes.yaml").exists(), "routes.yaml missing");
    assert!(
        mur_common::identity::private_key_dir(&dir)
            .join("identity.key")
            .exists(),
        "identity.key missing"
    );
    assert!(dir.join("identity.pub").exists(), "identity.pub missing");

    let p = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    assert!(p.contains("llm:"), "profile.yaml missing llm: block");
    assert!(
        p.contains("mode: off"),
        "profile.yaml missing llm.mode = off"
    );

    let r = std::fs::read_to_string(dir.join("routes.yaml")).unwrap();
    assert!(
        r.contains("default_route: coach"),
        "routes.yaml missing default_route: coach"
    );
}

#[test]
fn unknown_platform_errors() {
    // M-c2.0.2: `telegram` is now a recognised platform (returns a typed
    // "BotFather setup not yet wired" error). Use a still-unrecognised
    // platform name to assert the unknown-platform branch.
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "agent",
            "companion",
            "connector",
            "add",
            "discord_bridge",
            "--platform",
            "discord",
            "--default-route",
            "coach",
        ])
        .env("MUR_HOME", &mur_home)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected failure for unknown platform"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not supported"), "stderr was: {stderr}");
}

#[test]
fn duplicate_add_errors() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    let exe = env!("CARGO_BIN_EXE_mur");
    let go = || {
        Command::new(exe)
            .args([
                "agent",
                "companion",
                "connector",
                "add",
                "stub_bridge",
                "--platform",
                "stub",
                "--default-route",
                "coach",
            ])
            .env("MUR_HOME", &mur_home)
            .output()
            .unwrap()
    };
    let first = go();
    assert!(
        first.status.success(),
        "first add failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let dup = go();
    assert!(
        !dup.status.success(),
        "expected duplicate-add failure but got success"
    );
    let stderr = String::from_utf8_lossy(&dup.stderr);
    assert!(stderr.contains("already exists"), "stderr was: {stderr}");
}
