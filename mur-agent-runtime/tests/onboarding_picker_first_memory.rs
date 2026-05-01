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

#[test]
fn voice_template_both_placeholders_independent() {
    // Pin that {{FIRST_MEMORY}} and {{FIRST_MEMORY_PARAGRAPH}} substitute
    // independently in the same template — no cross-contamination from
    // substring matching.
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: Some("Taipei"),
        formality: "casual",
        extra_instructions: "",
    };
    let out = substitute_for_test(
        "verbatim={{FIRST_MEMORY}}, paragraph={{FIRST_MEMORY_PARAGRAPH}}",
        &input,
    );
    assert_eq!(out, "verbatim=Taipei, paragraph= Taipei");
}

#[test]
fn voice_template_first_memory_some_empty_collapses_like_none() {
    // An empty first-memory string is semantically not-set; treat it the
    // same as None so paragraph form doesn't leak a stray leading space.
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "David",
        first_memory: Some(""),
        formality: "casual",
        extra_instructions: "",
    };
    let out = substitute_for_test("Hi {{NAME_FOR_USER}}.{{FIRST_MEMORY_PARAGRAPH}}", &input);
    assert_eq!(out, "Hi David.");
}

#[test]
fn voice_template_user_supplied_field_does_not_inject_placeholder() {
    // Substitution-order safety: a user who types `{{FIRST_MEMORY}}` as their
    // preferred name must NOT see that token re-expanded on a later pass.
    let input = VoiceInput {
        relationship: Relationship::Friend,
        locale: "en-US",
        name_for_user: "{{FIRST_MEMORY}}",
        first_memory: Some("Sunday in Taipei"),
        formality: "casual",
        extra_instructions: "",
    };
    let out = substitute_for_test("Hi {{NAME_FOR_USER}}.", &input);
    assert_eq!(out, "Hi {{FIRST_MEMORY}}.");
}
