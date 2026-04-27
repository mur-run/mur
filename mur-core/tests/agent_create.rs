// Windows: gated — uses std::fs::read_link on a unix-style symlink the CLI
// creates, and depends on `std::os::unix::fs::symlink` succeeding without
// admin privileges (Windows symlinks require elevation by default).
#![cfg(unix)]

use mur_common::AgentProfile;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn agent_create_non_interactive_writes_profile_prompt_and_symlink() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let runtime_target = "/tmp/mur-agent-runtime-stub-for-test";

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", runtime_target)
        .args([
            "agent",
            "create",
            "agent_x",
            "--no-interactive",
            "--display-name",
            "X",
            "--model",
            "llama3.2:3b",
        ])
        .output()
        .expect("run mur");
    assert!(
        out.status.success(),
        "mur agent create failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // profile.yaml exists and parses
    let profile_path = mur_home.path().join("agents/agent_x/profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path).expect("profile.yaml");
    let profile: AgentProfile = serde_yaml_ng::from_str(&yaml).expect("parse AgentProfile");
    assert_eq!(profile.name, "agent_x");
    assert_eq!(profile.display_name, "X");
    assert_eq!(profile.model.name, "llama3.2:3b");
    assert!(matches!(
        profile.persona.category,
        mur_common::PersonaCategory::Custom
    ));
    let id = uuid::Uuid::parse_str(&profile.id).expect("uuid");
    assert_eq!(id.get_version_num(), 7, "profile.id must be UUIDv7");

    // sys_prompt.md exists
    let prompt_path = mur_home.path().join("agents/agent_x/sys_prompt.md");
    assert!(prompt_path.exists(), "sys_prompt.md missing");

    // symlink points to runtime target
    let symlink = bin_dir.path().join("mur_agent_agent_x");
    let link_target = std::fs::read_link(&symlink).expect("read_link");
    assert_eq!(link_target.to_string_lossy(), runtime_target);
}

/// E2 regression: `--model provider/model` shorthand routes to the right
/// provider for known providers; falls back to ollama for unknown prefixes
/// (e.g. HuggingFace `org/model`).
#[test]
fn agent_create_parses_provider_slash_model_shorthand() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let runtime_target = "/tmp/mur-agent-runtime-stub-for-test";
    let mur = env!("CARGO_BIN_EXE_mur");

    // anthropic/<model> → provider=anthropic
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", runtime_target)
        .args([
            "agent",
            "create",
            "anthropic_a",
            "--no-interactive",
            "--model",
            "anthropic/claude-opus-4-7",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let yaml = std::fs::read_to_string(
        mur_home.path().join("agents/anthropic_a/profile.yaml"),
    )
    .unwrap();
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(p.model.provider, "anthropic");
    assert_eq!(p.model.name, "claude-opus-4-7");

    // unknown prefix (HuggingFace style) → provider stays ollama, name kept whole
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", runtime_target)
        .args([
            "agent",
            "create",
            "hf_a",
            "--no-interactive",
            "--model",
            "meta-llama/Llama-3.2-3B",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let yaml = std::fs::read_to_string(mur_home.path().join("agents/hf_a/profile.yaml")).unwrap();
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(p.model.provider, "ollama");
    assert_eq!(p.model.name, "meta-llama/Llama-3.2-3B");
}

/// E2 regression: explicit `--provider` overrides any prefix in `--model`.
#[test]
fn agent_create_explicit_provider_wins_over_model_prefix() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    let runtime_target = "/tmp/mur-agent-runtime-stub-for-test";
    let mur = env!("CARGO_BIN_EXE_mur");

    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .env("MUR_AGENT_BIN_DIR", bin_dir.path())
        .env("MUR_AGENT_RUNTIME_BIN", runtime_target)
        .args([
            "agent",
            "create",
            "explicit_a",
            "--no-interactive",
            "--provider",
            "openai",
            "--model",
            "anthropic/foo",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let yaml = std::fs::read_to_string(
        mur_home.path().join("agents/explicit_a/profile.yaml"),
    )
    .unwrap();
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(p.model.provider, "openai");
    assert_eq!(p.model.name, "anthropic/foo");
}
