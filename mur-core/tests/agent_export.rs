// Windows: gated — drives the `mur agent export/import` CLI which spawns
// the runtime and depends on unix-style symlink + process pipe semantics.
#![cfg(unix)]

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

#[test]
fn export_pkg_via_cli_writes_tar_gz() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let out_dir = TempDir::new().unwrap();
    let out_path = out_dir.path().join("agent_x.murpkg");

    let mur = env!("CARGO_BIN_EXE_mur");
    let res = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args([
            "agent",
            "export",
            "agent_x",
            "--out",
            out_path.to_str().unwrap(),
            "--format",
            "pkg",
        ])
        .output()
        .expect("spawn mur export");
    assert!(
        res.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&res.stderr)
    );
    assert!(out_path.exists(), "package file should exist");
    // Sniff that it's a gzip stream (RFC 1952 magic 0x1f 0x8b).
    let bytes = std::fs::read(&out_path).unwrap();
    assert!(bytes.len() > 2);
    assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
}

#[test]
fn export_bin_unsupported_format_errors() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let mur = env!("CARGO_BIN_EXE_mur");
    let res = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args([
            "agent", "export", "agent_x", "--out", "/tmp/x", "--format", "wat",
        ])
        .output()
        .expect("spawn mur export");
    assert!(!res.status.success(), "bogus format should fail");
    let err = String::from_utf8_lossy(&res.stderr);
    assert!(err.contains("unsupported"), "stderr: {err}");
}

/// Full bin-format build is e2e and slow (drives `cargo build` for the
/// runtime with the embedded-agent feature). Marked #[ignore] so default
/// `cargo test` runs stay fast; opt-in with
/// `cargo test --test agent_export -- --ignored`.
#[test]
#[ignore]
fn export_bin_produces_self_contained_executable() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let out_dir = TempDir::new().unwrap();
    let out_path = out_dir.path().join("my_agent");

    let mur = env!("CARGO_BIN_EXE_mur");
    let res = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args([
            "agent",
            "export",
            "agent_x",
            "--out",
            out_path.to_str().unwrap(),
            "--format",
            "bin",
        ])
        .output()
        .expect("spawn mur export bin");
    assert!(
        res.status.success(),
        "bin export failed: {}",
        String::from_utf8_lossy(&res.stderr)
    );
    assert!(out_path.exists());
    let perms = std::fs::metadata(&out_path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(perms.mode() & 0o111 != 0, "binary should be executable");
    }
    let _ = perms;
}
