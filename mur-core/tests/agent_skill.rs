// Windows: gated — depends on `mur agent create` (unix symlink).
#![cfg(unix)]

use mur_common::AgentProfile;
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
fn skill_add_copies_file_and_appends_id() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("research.md");
    std::fs::write(&skill_src, "# Research\nbody").unwrap();

    let out = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let dest = mur_home.path().join("agents/agent_x/skills/research.md");
    assert!(dest.exists(), "skill file should have been copied");
    let p = read_profile(mur_home.path(), "agent_x");
    assert_eq!(p.skills, vec!["skills/research.md".to_string()]);
}

#[test]
fn skill_list_show_remove_roundtrip() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("research.md");
    std::fs::write(&skill_src, "body-only").unwrap();
    let _ = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );

    let list_out = run(mur_home.path(), &["agent", "skill", "list", "agent_x"]);
    assert!(list_out.status.success());
    let body = String::from_utf8(list_out.stdout).unwrap();
    assert!(body.contains("skills/research.md"));

    let show_out = run(
        mur_home.path(),
        &["agent", "skill", "show", "agent_x", "skills/research.md"],
    );
    assert!(
        show_out.status.success(),
        "{}",
        String::from_utf8_lossy(&show_out.stderr)
    );
    let shown = String::from_utf8(show_out.stdout).unwrap();
    assert!(shown.contains("body-only"));

    let rm_out = run(
        mur_home.path(),
        &["agent", "skill", "remove", "agent_x", "skills/research.md"],
    );
    assert!(
        rm_out.status.success(),
        "{}",
        String::from_utf8_lossy(&rm_out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(p.skills.is_empty());
    let dest = mur_home.path().join("agents/agent_x/skills/research.md");
    assert!(!dest.exists(), "orphaned skill file should be deleted");
}

/// E3 regression: `skill show` and `skill remove` accept the basename
/// (`research.md`) and the stem (`research`) in addition to the full
/// stored id (`skills/research.md`).
#[test]
fn skill_show_and_remove_accept_basename_and_stem() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let src = TempDir::new().unwrap();
    let skill_src = src.path().join("research.md");
    std::fs::write(&skill_src, "skill-body").unwrap();
    let _ = run(
        mur_home.path(),
        &[
            "agent",
            "skill",
            "add",
            "agent_x",
            skill_src.to_str().unwrap(),
        ],
    );

    // show by basename
    let out = run(
        mur_home.path(),
        &["agent", "skill", "show", "agent_x", "research.md"],
    );
    assert!(
        out.status.success(),
        "show by basename failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("skill-body"));

    // show by stem (without .md)
    let out = run(
        mur_home.path(),
        &["agent", "skill", "show", "agent_x", "research"],
    );
    assert!(
        out.status.success(),
        "show by stem failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("skill-body"));

    // remove by stem
    let out = run(
        mur_home.path(),
        &["agent", "skill", "remove", "agent_x", "research"],
    );
    assert!(
        out.status.success(),
        "remove by stem failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let p = read_profile(mur_home.path(), "agent_x");
    assert!(p.skills.is_empty(), "skill should be removed");
}
