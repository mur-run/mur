//! #2015: a runtime that refuses to start must exit with the refusal code
//! (which the systemd unit will not restart) and leave `start_refused.json`
//! behind (which the launchd plist's `KeepAlive` is gated on, and which
//! `mur agent status` reads). The supervisor's unit test covers the marker
//! writer; this drives the real binary, because the exit code is the half a
//! unit test cannot see.

use std::process::{Command, Stdio};

use mur_common::start_refusal::{self, EXIT_CODE};
use tempfile::TempDir;

fn run(mur_home: &std::path::Path, name: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mur-agent-runtime"))
        .env("MUR_HOME", mur_home)
        .args(["--profile", name, "start"])
        .stdin(Stdio::null())
        .output()
        .expect("spawn runtime")
}

#[test]
fn invalid_profile_exits_with_the_refusal_code_and_records_why() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("agents").join("agent_bad");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("profile.yaml"), ":: not yaml [").unwrap();

    let out = run(tmp.path(), "agent_bad");
    assert_eq!(
        out.status.code(),
        Some(EXIT_CODE),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = start_refusal::read(&home).expect("refusal marker written");
    assert_eq!(r.code, "profile_invalid");
}

#[test]
fn entitlement_change_exits_with_the_refusal_code_and_records_why() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("agents").join("agent_t");
    std::fs::create_dir_all(&home).unwrap();
    let yaml = include_str!("fixtures/profile_stdio.yaml");
    std::fs::write(home.join("profile.yaml"), yaml).unwrap();
    std::fs::write(home.join("sys_prompt.md"), "You are a test.").unwrap();
    // Pin entitlements that differ from what is on disk: the "edited outside
    // MUR" case.
    let mut pinned =
        mur_common::entitlements_pin::entitlements_from_yaml(yaml, &home.join("profile.yaml"))
            .unwrap();
    pinned.filesystem.write.push("/pinned-elsewhere".into());
    mur_common::entitlements_pin::write_pin(tmp.path(), "agent_t", &pinned).unwrap();

    let out = run(tmp.path(), "agent_t");
    assert_eq!(
        out.status.code(),
        Some(EXIT_CODE),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = start_refusal::read(&home).expect("refusal marker written");
    assert_eq!(r.code, "entitlements_unpinned");
    assert!(
        r.message.contains("mur agent perm reseal agent_t"),
        "{}",
        r.message
    );
}
