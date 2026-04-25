// Windows: gated — install-service emits launchd / systemd files only;
// the test relies on `mur agent create` succeeding (unix symlink) first.
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
fn install_service_dry_run_emits_platform_template() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .args(["agent", "install-service", "agent_x", "--dry-run"])
        .output()
        .expect("spawn mur install-service");
    assert!(
        out.status.success(),
        "install-service failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();

    #[cfg(target_os = "macos")]
    {
        assert!(body.contains("<plist"), "missing <plist>: {body}");
        assert!(
            body.contains("mur_agent_agent_x"),
            "plist should reference the BusyBox symlink: {body}"
        );
        assert!(body.contains("<string>start</string>"));
    }
    #[cfg(target_os = "linux")]
    {
        assert!(body.contains("[Unit]"));
        assert!(body.contains("[Service]"));
        assert!(
            body.contains("mur_agent_agent_x"),
            "ExecStart should reference the BusyBox symlink: {body}"
        );
        assert!(body.contains("ExecStart="));
    }
}
