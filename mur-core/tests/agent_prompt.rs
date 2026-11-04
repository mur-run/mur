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
        .args(["agent", "prompt", "show", "agent_x", "--authored"])
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

/// #2268: the default `show` prints what the agent actually receives — the
/// authored file first, then the runtime's static sections under a
/// `[runtime-injected]` marker, with the scratch dir resolved for THIS agent.
#[test]
fn prompt_show_default_appends_runtime_injected_sections() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
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
    assert!(body.starts_with("# Marker\nhello world"), "{body}");
    let marker = body.find("[runtime-injected]").expect("marker present");
    let rule = body.find("## Output locations").expect("rule present");
    assert!(rule > marker, "rule sits under the marker");
    let scratch = mur_home.path().join("tmp/agent_x");
    assert!(
        body.contains(&format!("`{}`", scratch.display())),
        "scratch dir resolved for this agent: {body}"
    );
    assert!(
        !body.contains("## Secrets available"),
        "no secrets configured, so no secrets section: {body}"
    );
    // The per-turn rule templates are shown with `{path}` unresolved, after
    // the static sections and before the closing note.
    let wd = body
        .find("## Working directory")
        .expect("working-dir template");
    assert!(wd > rule, "per-turn templates follow the static sections");
    assert!(
        body.contains("`{path}`"),
        "placeholder left unresolved: {body}"
    );
    assert!(
        body.contains("<project_instructions>"),
        "project-instructions template present: {body}"
    );
    let note = body.find("Also appended per turn").expect("note present");
    assert!(note > wd, "note comes last");
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
