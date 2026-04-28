//! In-memory voice composition. Reads embedded templates via
//! [`mur_common::companion::voice_template`], applies placeholder substitution.
//! Disk override lives in M2.3.

use mur_common::companion::{voice_template, Relationship};

pub struct VoiceInput<'a> {
    pub relationship: Relationship,
    pub locale: &'a str,
    pub name_for_user: &'a str,
    pub formality: &'a str,
    pub extra_instructions: &'a str,
}

pub fn compose_in_memory(input: VoiceInput<'_>) -> String {
    let (locale_used, tpl) = voice_template::resolve_locale(&input.relationship, input.locale);
    tpl.replace("{{NAME_FOR_USER}}", input.name_for_user)
        .replace("{{FORMALITY}}", input.formality)
        .replace("{{EXTRA_INSTRUCTIONS}}", input.extra_instructions)
        .replace("{{LOCALE}}", &locale_used)
}
