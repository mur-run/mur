use mur_agent_runtime::companion::voice::{compose_in_memory, compose_with_overrides, VoiceInput};
use mur_common::companion::Relationship;
use tempfile::TempDir;

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

#[test]
fn per_agent_disk_override_wins() {
    let home = TempDir::new().unwrap();
    let agent_dir = home.path().join(".mur/agents/x");
    let templates_dir = agent_dir.join("companion/templates");
    std::fs::create_dir_all(&templates_dir).unwrap();
    std::fs::write(
        templates_dir.join("friend.zh-TW.md"),
        "OVERRIDE {{NAME_FOR_USER}} {{LOCALE}}",
    )
    .unwrap();

    let body = compose_with_overrides(
        Some(&agent_dir),
        Some(&home.path().join(".mur")),
        VoiceInput {
            relationship: Relationship::Friend,
            locale: "zh-TW",
            name_for_user: "Bob",
            formality: "casual",
            extra_instructions: "",
        },
    );
    assert!(body.starts_with("OVERRIDE Bob zh-TW"));
}

#[test]
fn user_dir_override_used_when_no_agent_override() {
    let home = TempDir::new().unwrap();
    let agent_dir = home.path().join(".mur/agents/y");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let user_templates = home.path().join(".mur/companion/templates");
    std::fs::create_dir_all(&user_templates).unwrap();
    std::fs::write(
        user_templates.join("coach.en-US.md"),
        "USER OVERRIDE {{NAME_FOR_USER}}",
    )
    .unwrap();

    let body = compose_with_overrides(
        Some(&agent_dir),
        Some(&home.path().join(".mur")),
        VoiceInput {
            relationship: Relationship::Coach,
            locale: "en-US",
            name_for_user: "Alice",
            formality: "neutral",
            extra_instructions: "",
        },
    );
    assert!(body.starts_with("USER OVERRIDE Alice"));
}

#[test]
fn falls_through_to_embedded_when_no_disk_override() {
    let home = TempDir::new().unwrap();
    let body = compose_with_overrides(
        Some(&home.path().join(".mur/agents/z")),
        Some(&home.path().join(".mur")),
        VoiceInput {
            relationship: Relationship::Friend,
            locale: "zh-TW",
            name_for_user: "Carol",
            formality: "casual",
            extra_instructions: "",
        },
    );
    assert!(body.contains("Carol"));
    assert!(!body.contains("{{NAME_FOR_USER}}"));
}
