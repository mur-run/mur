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
    assert!(dir.join("identity.key").exists(), "identity.key missing");
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
