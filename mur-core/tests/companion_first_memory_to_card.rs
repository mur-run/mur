//! M2.7.2 — `build_card_from_profile` threads the companion's first memory
//! into `MurCard.extensions.mur.first_memory` and falls back to the agent
//! profile's `name` when no display name was supplied.
//!
//! `AgentProfile` does not derive `Default`, so we build profiles by
//! parsing the shared P0a fixture (same pattern as
//! `mur-core/tests/companion_init_5step.rs`).

use mur_common::agent::{AgentProfile, FirstMemory, OnboardingState};

/// Load the shared P0a fixture and rename it so each test starts from a
/// known-parseable baseline. Mirrors the helper in
/// `mur-core/tests/companion_init_5step.rs`.
fn fixture_with_name(name: &str) -> AgentProfile {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
    let yaml = std::fs::read_to_string(&p).expect("fixture profile_p0a_minimal.yaml must exist");
    let yaml = yaml.replace("name: agent_test", &format!("name: {name}"));
    serde_yaml_ng::from_str(&yaml).expect("fixture must parse as AgentProfile")
}

#[test]
fn build_card_includes_first_memory_when_present() {
    let mut p = fixture_with_name("test");
    p.companion.onboarding = OnboardingState {
        completed_at: Some(chrono::Utc::now()),
        version: 1,
        agent_display_name: Some("Mochi".into()),
        first_memory: Some(FirstMemory {
            text: "Sunday in Taipei".into(),
            established_at: chrono::Utc::now(),
        }),
    };

    let card = mur_core::cmd::agent_export::card::build_card_from_profile(&p);
    assert_eq!(card.data.name, "Mochi");
    assert_eq!(card.data.description, "");
    assert_eq!(card.spec, "murcard_v1");
    assert_eq!(card.spec_version, "1.0");

    let yaml = serde_yaml_ng::to_string(&card).unwrap();
    assert!(
        yaml.contains("Sunday in Taipei"),
        "first_memory text missing from serialized card:\n{yaml}"
    );
    assert!(
        yaml.contains("first_memory:"),
        "first_memory key missing from serialized card:\n{yaml}"
    );
}

#[test]
fn build_card_omits_first_memory_when_absent() {
    let p = fixture_with_name("blank");
    // Default companion onboarding has no display name and no first memory.
    let card = mur_core::cmd::agent_export::card::build_card_from_profile(&p);
    // Falls back to profile.name when agent_display_name is None.
    assert_eq!(card.data.name, "blank");
    assert!(card.extensions.is_none());

    let yaml = serde_yaml_ng::to_string(&card).unwrap();
    assert!(
        !yaml.contains("first_memory:"),
        "first_memory must not appear when absent:\n{yaml}"
    );
}
