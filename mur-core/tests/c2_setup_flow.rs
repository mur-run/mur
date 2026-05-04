//! Track C2 / M-c2.0.2: `mur agent companion connector add --platform telegram`
//! integration test.
//!
//! M-c2.0 only locks in the schema and CLI plumbing; the BotFather setup UX
//! itself lands in M-c2.1. The arm must therefore exit non-zero with a typed
//! error message that downstream tracking can grep for.
//!
//! Windows: gated for parity with `connector_add_stub.rs`.
#![cfg(unix)]

use std::process::Command;
use tempfile::TempDir;

#[test]
fn telegram_arm_returns_typed_error_pre_m_c2_1() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "agent",
            "companion",
            "connector",
            "add",
            "tg",
            "--platform",
            "telegram",
            "--default-route",
            "coach",
        ])
        .env("MUR_HOME", &mur_home)
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "expected non-zero exit (M-c2.0.2 stub): stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("BotFather setup not yet wired"),
        "stderr={stderr}"
    );
}
