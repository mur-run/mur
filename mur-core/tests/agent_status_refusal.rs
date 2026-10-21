//! #2015 acceptance: "`mur agent status <name>` shows the refusal reason and
//! the reseal command" — for both refusals the runtime records.
//!
//! The marker is written with the same `start_refusal` API the runtime uses
//! (`mur-agent-runtime/tests/start_refusal.rs` proves the real binary writes
//! it); this test owns the CLI half.
#![cfg(unix)]

use std::process::Command;

use mur_common::start_refusal::{StartRefusal, write};
use tempfile::TempDir;

fn mur(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mur"))
        .env("MUR_HOME", home)
        .env("MUR_AGENT_BIN_DIR", home.join("bin"))
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(args)
        .output()
        .expect("spawn mur")
}

fn create(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    let out = mur(home, &["agent", "create", name, "--no-interactive"]);
    assert!(
        out.status.success(),
        "create {name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    home.join("agents").join(name)
}

fn status(home: &std::path::Path, name: &str) -> (bool, String) {
    let out = mur(home, &["agent", "status", name]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

#[test]
fn status_shows_an_entitlement_refusal_and_the_reseal_command() {
    let tmp = TempDir::new().unwrap();
    let home = create(tmp.path(), "bob");
    write(
        &home,
        &StartRefusal::from_line(
            "error[entitlements_unpinned]: bob's profile.yaml entitlements changed outside MUR (filesystem.write)",
            1,
        ),
    )
    .unwrap();

    let (ok, text) = status(tmp.path(), "bob");
    assert!(ok, "{text}");
    assert!(text.contains("Active: stopped"), "{text}");
    assert!(text.contains("entitlements_unpinned"), "{text}");
    assert!(
        text.contains("changed outside MUR (filesystem.write)"),
        "{text}"
    );
    assert!(text.contains("mur agent perm reseal bob"), "{text}");
}

#[test]
fn status_shows_an_invalid_profile_refusal_instead_of_not_found() {
    let tmp = TempDir::new().unwrap();
    let home = create(tmp.path(), "eve");
    std::fs::write(home.join("profile.yaml"), ":: not yaml [").unwrap();
    write(
        &home,
        &StartRefusal::from_line("error[profile_invalid]: missing field `schema`", 1),
    )
    .unwrap();

    let (ok, text) = status(tmp.path(), "eve");
    assert!(ok, "{text}");
    assert!(!text.contains("not found"), "{text}");
    assert!(text.contains("profile_invalid"), "{text}");
    assert!(text.contains("missing field `schema`"), "{text}");
}

#[test]
fn an_unparsable_profile_without_a_refusal_is_still_not_found() {
    let tmp = TempDir::new().unwrap();
    let home = create(tmp.path(), "zed");
    std::fs::write(home.join("profile.yaml"), ":: not yaml [").unwrap();

    let (ok, text) = status(tmp.path(), "zed");
    assert!(!ok, "{text}");
    assert!(text.contains("not found"), "{text}");
}

#[test]
fn a_healthy_stopped_agent_shows_no_refusal() {
    let tmp = TempDir::new().unwrap();
    create(tmp.path(), "amy");
    let (ok, text) = status(tmp.path(), "amy");
    assert!(ok, "{text}");
    assert!(!text.contains("Refused"), "{text}");
}
