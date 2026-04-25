// Windows: gated — depends on `mur agent create` (unix symlink).
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
fn prompt_show_prints_sys_prompt_contents() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    // Overwrite the default prompt with a known marker.
    let prompt_path = mur_home.path().join("agents/agent_x/sys_prompt.md");
    std::fs::write(&prompt_path, "# Marker\nhello world").unwrap();

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args(["agent", "prompt", "show", "agent_x"])
        .output()
        .expect("spawn mur prompt show");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert_eq!(body, "# Marker\nhello world");
}

#[test]
fn prompt_set_writes_literal_and_creates_bak() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let prompt_path = mur_home.path().join("agents/agent_x/sys_prompt.md");
    let bak_path = mur_home.path().join("agents/agent_x/sys_prompt.md.bak");
    assert!(prompt_path.exists() && !bak_path.exists());

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args(["agent", "prompt", "set", "agent_x", "hello"])
        .output()
        .expect("spawn mur prompt set");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), "hello");
    assert!(bak_path.exists(), "backup .bak should exist after set");
}

#[test]
fn prompt_set_from_file_reads_file_content() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let prompt_path = mur_home.path().join("agents/agent_x/sys_prompt.md");
    let src = TempDir::new().unwrap();
    let src_file = src.path().join("p.md");
    std::fs::write(&src_file, "from-file").unwrap();

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args([
            "agent",
            "prompt",
            "set",
            "agent_x",
            "-f",
            src_file.to_str().unwrap(),
        ])
        .output()
        .expect("spawn mur prompt set -f");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&prompt_path).unwrap(), "from-file");
}
