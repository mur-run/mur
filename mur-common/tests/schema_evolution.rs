//! Asserts every future profile change still loads v1_frozen.yaml.
//! See R12 in docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md.

use mur_common::agent::AgentProfile;

#[test]
fn v1_frozen_profile_still_loads() {
    let yaml = std::fs::read_to_string("../tests/fixtures/profile/v1_frozen.yaml")
        .expect("v1_frozen.yaml must exist — DO NOT DELETE OR EDIT");
    let _p: AgentProfile = serde_yaml_ng::from_str(&yaml)
        .expect("v1_frozen profile must remain readable across all future schema changes");
}
