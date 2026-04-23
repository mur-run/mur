use mur_agent_runtime::entitlements::{WarningKind, detect_warnings, preset_for_category};
use mur_common::agent::{NetworkOutboundMode, SpawnMode};
use mur_common::{AgentProfile, PersonaCategory};

fn sample_profile_unrestricted() -> AgentProfile {
    let yaml = include_str!("fixtures/profile_unrestricted.yaml");
    serde_yaml_ng::from_str(yaml).expect("fixture parse")
}

#[test]
fn warns_on_unrestricted_network() {
    let p = sample_profile_unrestricted();
    let warnings = detect_warnings(&p);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w.kind, WarningKind::UnrestrictedNetwork))
    );
}

#[test]
fn warns_on_empty_deny_list() {
    let mut p = sample_profile_unrestricted();
    p.entitlements.filesystem.deny.clear();
    let warnings = detect_warnings(&p);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w.kind, WarningKind::EmptyFilesystemDeny))
    );
}

#[test]
fn warns_on_spawn_any() {
    let mut p = sample_profile_unrestricted();
    p.entitlements.processes.spawn.mode = SpawnMode::Any;
    let warnings = detect_warnings(&p);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w.kind, WarningKind::OpenProcessSpawn))
    );
}

#[test]
fn research_preset_restricted_no_hosts() {
    let preset = preset_for_category(PersonaCategory::Research);
    assert_eq!(preset.network_mode, NetworkOutboundMode::Restricted);
    assert!(preset.network_hosts.is_empty());
    assert!(
        preset
            .process_allowed
            .contains(&"agent-browser".to_string())
    );
}

#[test]
fn notify_preset_unrestricted() {
    let preset = preset_for_category(PersonaCategory::Notify);
    assert_eq!(preset.network_mode, NetworkOutboundMode::Unrestricted);
}
