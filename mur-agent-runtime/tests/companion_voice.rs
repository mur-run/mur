use mur_agent_runtime::companion::voice::{compose_in_memory, VoiceInput};
use mur_common::companion::Relationship;

#[test]
fn placeholders_replaced() {
    let v = compose_in_memory(VoiceInput {
        relationship: Relationship::Friend,
        locale: "zh-TW",
        name_for_user: "David",
        formality: "casual",
        extra_instructions: "",
    });
    assert!(v.contains("David"));
    assert!(!v.contains("{{NAME_FOR_USER}}"));
    assert!(!v.contains("{{FORMALITY}}"));
    assert!(!v.contains("{{LOCALE}}"));
    assert!(!v.contains("{{EXTRA_INSTRUCTIONS}}"));
    assert!(v.contains("zh-TW"));
}

#[test]
fn unknown_locale_falls_back_to_en_us() {
    let v = compose_in_memory(VoiceInput {
        relationship: Relationship::Mentor,
        locale: "de-DE",
        name_for_user: "Hans",
        formality: "neutral",
        extra_instructions: "",
    });
    assert!(v.contains("Hans"));
    assert!(v.to_lowercase().contains("voice rules"));
}
