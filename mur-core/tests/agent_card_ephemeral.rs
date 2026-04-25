//! Ephemeral `mur agent card` path: when the agent isn't running, mur
//! should spawn the runtime briefly, fetch its card, and tear it down.
//!
//! The test is gated on the runtime binary being present alongside the
//! mur binary in the cargo target dir; if it's absent (e.g. running
//! `cargo test -p mur-core` in isolation without first building the
//! runtime), the test is skipped with a `println!` so default cargo test
//! runs stay green.
//!
//! Windows: gated to cfg(unix) — the ephemeral runtime fork pipes stdio
//! and Windows `Command::output()` doesn't reap the child cleanly,
//! causing the test to hang to the GitHub Actions 6h job timeout.

#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn locate_runtime_binary() -> Option<PathBuf> {
    // CARGO_BIN_EXE_mur is <target>/<profile>/mur. Sibling mur-agent-runtime
    // gets built only when explicitly requested or pulled in by another test
    // that exercises the runtime crate.
    let mur = std::path::Path::new(env!("CARGO_BIN_EXE_mur"));
    let candidate = mur.parent()?.join(if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    });
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

#[test]
fn card_ephemeral_invokes_runtime_when_not_running() {
    let Some(runtime_bin) = locate_runtime_binary() else {
        eprintln!("(skipping — mur-agent-runtime binary not present in target dir)");
        return;
    };

    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let mur = env!("CARGO_BIN_EXE_mur");

    // Create the agent (this writes profile.yaml + sys_prompt.md).
    let create = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", &runtime_bin)
        .args(["agent", "create", "agent_e", "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );

    // No running.lock — cmd_card must fall through to ephemeral runtime.
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", &runtime_bin)
        .args(["agent", "card", "agent_e"])
        .output()
        .expect("spawn mur card");
    assert!(
        out.status.success(),
        "card failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(body.trim()).expect("card stdout must be JSON");
    assert_eq!(v["name"], "agent_e");
    assert_eq!(v["protocolVersion"], "a2a/0.3");
}
