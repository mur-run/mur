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
