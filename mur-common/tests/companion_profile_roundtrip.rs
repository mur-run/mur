use mur_common::agent::AgentProfile;
use mur_common::companion::Relationship;

#[test]
fn profile_without_companion_block_loads_with_defaults() {
    let yaml = std::fs::read_to_string("../tests/fixtures/profile/v1_minimum.yaml").unwrap();
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert!(!p.companion.enabled);
    assert!(matches!(p.companion.relationship, Relationship::Friend));
    assert_eq!(p.companion.proactive.daily_cap, 3);
}

#[test]
fn companion_roundtrip_preserves_all_fields() {
    let mut p: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string("../tests/fixtures/profile/v1_minimum.yaml").unwrap(),
    )
    .unwrap();
    p.companion.enabled = true;
    p.companion.locale = "zh-TW".into();
    p.companion.relationship = Relationship::Coach;
    let s = serde_yaml_ng::to_string(&p).unwrap();
    let p2: AgentProfile = serde_yaml_ng::from_str(&s).unwrap();
    assert_eq!(p, p2);
}
