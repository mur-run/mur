use super::*;

#[test]
fn starter_fleet_run_seeds_concierge_deep_research() {
    let s = starter_fleet_run();
    assert_eq!(s.agents, vec!["mur"]);
    assert_eq!(s.fleets, vec!["deep-research"]);
}

#[test]
fn append_fleet_run_respects_existing_key_and_foreign_blocks() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("config.yaml");

    // Absent → appended, foreign block preserved.
    std::fs::write(&p, "research_gateway:\n  brave_api_key: \"x\"").unwrap();
    assert!(append_fleet_run_if_absent(&p).unwrap());
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("research_gateway:"), "foreign block kept");
    assert!(text.contains("fleet_run:"));
    let cfg: mur_common::config::Config = serde_yaml_ng::from_str(&text).unwrap();
    assert_eq!(cfg.fleet_run.agents, vec!["mur"]);
    assert_eq!(cfg.fleet_run.fleets, vec!["deep-research"]);

    // Present (user-authored) → untouched.
    std::fs::write(&p, "fleet_run:\n  agents: [custom]\n  fleets: []\n").unwrap();
    assert!(!append_fleet_run_if_absent(&p).unwrap());
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(text.matches("fleet_run:").count(), 1);
    assert!(text.contains("custom"));
}

#[test]
fn hook_scripts_use_unified_entry() {
    assert!(
        HOOK_SCRIPT_PROMPT.contains("mur hook prompt"),
        "on-prompt.sh must call mur hook prompt"
    );
    assert!(
        !HOOK_SCRIPT_PROMPT.contains("mur context"),
        "on-prompt.sh must NOT call mur context"
    );
    assert!(
        HOOK_SCRIPT_TOOL.contains("mur hook tool"),
        "on-tool.sh must call mur hook tool"
    );
    assert!(
        HOOK_SCRIPT_STOP.contains("mur hook stop"),
        "on-stop.sh must call mur hook stop"
    );
    assert!(
        HOOK_SCRIPT_SESSION_START.contains("mur hook session-start"),
        "on-session-start.sh must call mur hook session-start"
    );
    assert!(
        HOOK_SCRIPT_PROMPT.contains("v7"),
        "hook scripts must be version v7"
    );
}
