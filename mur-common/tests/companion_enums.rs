use mur_common::companion::{Formality, Relationship, Signal, Situation};

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
