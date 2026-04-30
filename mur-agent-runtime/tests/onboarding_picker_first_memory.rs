use mur_agent_runtime::companion::voice::{VoiceInput, substitute_for_test};
use mur_common::companion::Relationship;

#[test]
fn voice_template_substitutes_first_memory() {
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: Some("Sunday in Taipei"),
        formality: "casual",
        extra_instructions: "",
    };
    let out = substitute_for_test("User mentioned: {{FIRST_MEMORY}}", &input);
    assert_eq!(out, "User mentioned: Sunday in Taipei");
}

#[test]
fn voice_template_first_memory_none_collapses() {
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: None,
        formality: "casual",
        extra_instructions: "",
    };
    let out = substitute_for_test("Hi {{NAME_FOR_USER}}.{{FIRST_MEMORY_PARAGRAPH}}", &input);
    assert_eq!(out, "Hi David.");
}

#[test]
fn voice_template_first_memory_paragraph_form() {
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: Some("Sunday in Taipei"),
        formality: "casual",
        extra_instructions: "",
    };
    let out = substitute_for_test("Hi {{NAME_FOR_USER}}.{{FIRST_MEMORY_PARAGRAPH}}", &input);
    // Paragraph form prepends a space so the expansion reads naturally:
    assert_eq!(out, "Hi David. Sunday in Taipei");
}
