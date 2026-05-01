//! Golden snapshots for voice.md composition.
//!
//! Each `(relationship × locale)` combination from
//! `mur_common::companion::voice_template::all_templates()` produces one
//! snapshot. Update with `cargo insta review`; CI enforces stale snapshots
//! via `INSTA_UPDATE=no`.

use mur_agent_runtime::companion::voice::{VoiceInput, compose_in_memory};
use mur_common::companion::{Relationship, voice_template};

fn snapshot_name(rel: &Relationship, locale: &str) -> String {
    let r = relationship_segment(rel);
    format!("voice_{}_{}", r, locale.replace('-', "_"))
}

fn sentinel_input(rel: Relationship, locale: &'static str) -> VoiceInput<'static> {
    VoiceInput {
        relationship: rel,
        locale,
        name_for_user: "TEST_USER",
        first_memory: None,
        formality: "polite",
        extra_instructions: "TEST_EXTRA_INSTRUCTIONS",
    }
}

fn relationship_segment(r: &Relationship) -> &'static str {
    match r {
        Relationship::Friend => "friend",
        Relationship::Coach => "coach",
        Relationship::AccountabilityBuddy => "accountability_buddy",
        Relationship::Mentor => "mentor",
    }
}

#[test]
fn voice_snapshots_all_relationship_locale_combos() {
    for (rel, locale, _body) in voice_template::all_templates() {
        let input = sentinel_input(rel.clone(), locale);
        let composed = compose_in_memory(input);
        let snap_name = snapshot_name(&rel, locale);
        insta::assert_snapshot!(snap_name, composed);
    }
}

#[test]
fn sys_prompt_snapshots_all_combos() {
    // For Phase 1.1 the "sys_prompt" is the same as the composed voice.md.
    // (Future phases will layer additional content; the snapshot will reveal
    // when that happens.)
    for (rel, locale, _body) in voice_template::all_templates() {
        let input = sentinel_input(rel.clone(), locale);
        let composed = compose_in_memory(input);
        let snap_name = format!(
            "sys_prompt_{}_{}",
            relationship_segment(&rel),
            locale.replace('-', "_")
        );
        insta::assert_snapshot!(snap_name, composed);
    }
}
