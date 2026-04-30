//! In-memory voice composition. Reads embedded templates via
//! [`mur_common::companion::voice_template`], applies placeholder substitution.
//! Disk override (per-agent → user-wide → embedded) lives in [`compose_with_overrides`].

use mur_common::companion::{Relationship, voice_template};
use std::path::Path;

pub struct VoiceInput<'a> {
    pub relationship: Relationship,
    pub locale: &'a str,
    pub name_for_user: &'a str,
    /// Optional onboarding "first memory" phrase. When `Some`, expands the
    /// `{{FIRST_MEMORY}}` placeholder verbatim and `{{FIRST_MEMORY_PARAGRAPH}}`
    /// with a leading space so it joins surrounding prose naturally. When
    /// `None`, both placeholders collapse to the empty string.
    pub first_memory: Option<&'a str>,
    pub formality: &'a str,
    pub extra_instructions: &'a str,
}

pub fn compose_in_memory(input: VoiceInput<'_>) -> String {
    let (locale_used, tpl) = voice_template::resolve_locale(&input.relationship, input.locale);
    apply_placeholders(tpl, &locale_used, &input)
}

/// Compose voice with disk-override lookup chain.
///
/// Order: per-agent → user-wide → embedded.
/// `agent_dir` is the agent's directory (e.g. `~/.mur/agents/<name>`); function reads
/// `agent_dir/companion/templates/{relationship}.{locale}.md`.
/// `mur_root` is the mur home (e.g. `~/.mur`); reads
/// `mur_root/companion/templates/{relationship}.{locale}.md`.
/// Pass `None` for either to skip that layer.
///
/// Disk overrides use the *requested* locale verbatim (no language-only / en-US
/// fallback at the disk layer); if no disk override exists, falls through to
/// [`compose_in_memory`] which has the embedded fallback chain.
pub fn compose_with_overrides(
    agent_dir: Option<&Path>,
    mur_root: Option<&Path>,
    input: VoiceInput<'_>,
) -> String {
    let rel_str = relationship_filename_segment(&input.relationship);
    let filename = format!("{rel_str}.{}.md", input.locale);

    if let Some(d) = agent_dir {
        let p = d.join("companion/templates").join(&filename);
        if let Some(body) = read_template_file(&p) {
            return apply_placeholders(&body, input.locale, &input);
        }
    }
    if let Some(d) = mur_root {
        let p = d.join("companion/templates").join(&filename);
        if let Some(body) = read_template_file(&p) {
            return apply_placeholders(&body, input.locale, &input);
        }
    }
    compose_in_memory(input)
}

fn relationship_filename_segment(r: &Relationship) -> &'static str {
    match r {
        Relationship::Friend => "friend",
        Relationship::Coach => "coach",
        Relationship::AccountabilityBuddy => "accountability_buddy",
        Relationship::Mentor => "mentor",
    }
}

fn read_template_file(p: &Path) -> Option<String> {
    let body = std::fs::read_to_string(p).ok()?;
    if body.trim().is_empty() {
        return None;
    }
    Some(body)
}

fn apply_placeholders(tpl: &str, locale_used: &str, input: &VoiceInput<'_>) -> String {
    let mut tpl = tpl
        .replace("{{NAME_FOR_USER}}", input.name_for_user)
        .replace("{{FORMALITY}}", input.formality)
        .replace("{{EXTRA_INSTRUCTIONS}}", input.extra_instructions)
        .replace("{{LOCALE}}", locale_used);
    match input.first_memory {
        Some(fm) => {
            tpl = tpl.replace("{{FIRST_MEMORY}}", fm);
            tpl = tpl.replace("{{FIRST_MEMORY_PARAGRAPH}}", &format!(" {fm}"));
        }
        None => {
            tpl = tpl.replace("{{FIRST_MEMORY}}", "");
            tpl = tpl.replace("{{FIRST_MEMORY_PARAGRAPH}}", "");
        }
    }
    tpl
}

/// Test-only helper exposing the placeholder substitution logic directly so
/// integration tests can exercise `{{FIRST_MEMORY}}` / `{{FIRST_MEMORY_PARAGRAPH}}`
/// against arbitrary template strings without going through the embedded
/// template resolution.
#[doc(hidden)]
pub fn substitute_for_test(tpl: &str, input: &VoiceInput<'_>) -> String {
    apply_placeholders(tpl, input.locale, input)
}
