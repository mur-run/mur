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
