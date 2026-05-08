use mur_agent_runtime::hooks::builder::build_chain;
use mur_common::agent::AgentProfile;

fn base_profile() -> AgentProfile {
    let yaml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bare_profile.yaml"
    ))
    .unwrap();
    serde_yaml_ng::from_str(&yaml).unwrap()
}

fn tmp_dirs() -> (tempfile::TempDir, tempfile::TempDir) {
    (
        tempfile::TempDir::new().unwrap(),
        tempfile::TempDir::new().unwrap(),
    )
}

#[test]
fn mandatory_handlers_always_present() {
    let profile = base_profile();
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    let names = chain.names();
    assert_eq!(names[0], "TelemetryHook", "TelemetryHook must be first");
    assert_eq!(names[1], "B0SafetyHook", "B0SafetyHook must be second");
}

#[test]
fn ledger_on_by_default() {
    let profile = base_profile();
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(
        chain.names().contains(&"LedgerHook"),
        "LedgerHook on by default"
    );
}

#[test]
fn ledger_disabled_via_hooks_config() {
    let mut profile = base_profile();
    profile.hooks.ledger = false;
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(
        !chain.names().contains(&"LedgerHook"),
        "LedgerHook must be absent"
    );
}

#[test]
fn companion_voice_auto_wires_when_companion_enabled() {
    let mut profile = base_profile();
    profile.companion.enabled = true;
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(
        chain.names().contains(&"CompanionVoiceHook"),
        "CompanionVoiceHook must auto-wire when companion.enabled = true"
    );
}

#[test]
fn companion_voice_explicit_override_when_companion_disabled() {
    let mut profile = base_profile();
    profile.companion.enabled = false;
    profile.hooks.companion_voice = Some(true);
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(
        chain.names().contains(&"CompanionVoiceHook"),
        "hooks.companion_voice=true must override companion.enabled=false"
    );
}

#[test]
fn voice_input_suppressed_when_model_absent() {
    let mut profile = base_profile();
    profile.voice.enabled = true;
    // mur_tmp has no voices/ dir → model not found → handler skipped gracefully
    let (agent_tmp, mur_tmp) = tmp_dirs();
    let chain = build_chain(&profile, agent_tmp.path(), mur_tmp.path());
    assert!(
        !chain.names().contains(&"VoiceInputHook"),
        "VoiceInputHook skipped gracefully when model absent"
    );
}
