use mur_common::companion::{Formality, Relationship, Signal, Situation};
use mur_common::agent::FirstMemory;
use chrono::{TimeZone, Utc};

#[test]
fn relationship_default_is_friend() {
    assert!(matches!(Relationship::default(), Relationship::Friend));
}

#[test]
fn relationship_serde_roundtrip() {
    let r = Relationship::Coach;
    let s = serde_json::to_string(&r).unwrap();
    assert_eq!(s, "\"coach\"");
    let r2: Relationship = serde_json::from_str(&s).unwrap();
    assert!(matches!(r2, Relationship::Coach));
}

#[test]
fn situation_known_variants() {
    let s: Situation = serde_json::from_str("\"morning_greeting\"").unwrap();
    assert!(matches!(s, Situation::MorningGreeting));
}

#[test]
fn formality_default_is_neutral() {
    // smoke-check: ensure Formality is in scope and default is Neutral per spec
    assert!(matches!(Formality::default(), Formality::Neutral));
}

#[test]
fn signal_serde_roundtrip() {
    // smoke-check: ensure Signal is in scope and serializes snake_case
    let s = serde_json::to_string(&Signal::Positive).unwrap();
    assert_eq!(s, "\"positive\"");
}

#[test]
fn first_memory_yaml_roundtrip() {
    let fm = FirstMemory {
        text: "We met on a Sunday in Taipei.".into(),
        established_at: Utc.with_ymd_and_hms(2026, 4, 30, 14, 13, 0).unwrap(),
    };
    let s = serde_yaml_ng::to_string(&fm).unwrap();
    assert!(s.contains("text:"));
    assert!(s.contains("established_at:"));
    let back: FirstMemory = serde_yaml_ng::from_str(&s).unwrap();
    assert_eq!(back, fm);
}

#[test]
fn onboarding_state_pre_d2_yaml_still_deserializes() {
    // A profile written before M2.1.1 has no agent_display_name / first_memory
    // fields; #[serde(default)] must let it round-trip cleanly with the new
    // optional fields = None.
    let yaml = "completed_at: null\nversion: 0\n";
    let s: mur_common::agent::OnboardingState = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(s.completed_at.is_none());
    assert_eq!(s.version, 0);
    assert!(s.agent_display_name.is_none());
    assert!(s.first_memory.is_none());
}

#[test]
fn proactive_tiers_helper() {
    use mur_common::agent::{CompanionConfig, ProactiveTier};
    let mut c = CompanionConfig::default();
    c.enabled = true;
    let t = ProactiveTier::from_config(&c);
    assert_eq!(t, ProactiveTier::WarmOnly);

    c.rhythm.enabled = true;
    let t = ProactiveTier::from_config(&c);
    assert_eq!(t, ProactiveTier::WarmAndBehavior);

    c.proactive.enabled = true;
    let t = ProactiveTier::from_config(&c);
    assert_eq!(t, ProactiveTier::All);
}

#[test]
fn proactive_tiers_apply_round_trip() {
    use mur_common::agent::{CompanionConfig, ProactiveTier};
    let mut c = CompanionConfig::default();
    ProactiveTier::All.apply(&mut c);
    assert!(c.enabled && c.rhythm.enabled && c.proactive.enabled);
    ProactiveTier::WarmOnly.apply(&mut c);
    assert!(c.enabled && !c.rhythm.enabled && !c.proactive.enabled);
    ProactiveTier::Off.apply(&mut c);
    assert!(!c.enabled && !c.rhythm.enabled && !c.proactive.enabled);
}

#[test]
fn proactive_tier_apply_then_from_config_is_identity() {
    use mur_common::agent::{CompanionConfig, ProactiveTier};
    for t in [
        ProactiveTier::Off,
        ProactiveTier::WarmOnly,
        ProactiveTier::WarmAndBehavior,
        ProactiveTier::All,
    ] {
        let mut c = CompanionConfig::default();
        t.apply(&mut c);
        assert_eq!(ProactiveTier::from_config(&c), t, "round-trip failed for {t:?}");
    }
}
