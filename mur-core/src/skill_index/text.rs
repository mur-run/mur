use mur_common::skill::manifest::{Skill, SkillManifest};

/// Build the canonical text we embed for a skill from its full `Skill` wrapper.
pub fn embed_text(skill: &Skill) -> String {
    embed_manifest(&skill.manifest)
}

/// Build the canonical text from a `SkillManifest` alone (no wrapper needed).
/// Stable: changing this invalidates every embedded chunk and requires
/// `mur skill reindex-vec`.
///
/// Format (newline-joined, no trailing newline):
///   <name>
///   <description>
///   <abstract>
///   <trigger_keywords joined by spaces, sorted>
///   <first_procedure_step_description if any>
///
/// Order matters: name and description dominate the embedding, abstract
/// adds semantic context, triggers cover keyword-match cases.
pub fn embed_manifest(m: &SkillManifest) -> String {
    let mut parts = vec![
        m.name.clone(),
        m.description.clone(),
        m.content.r#abstract.clone(),
    ];

    let mut triggers: Vec<&str> = m
        .triggers
        .iter()
        .filter_map(|t| t.exact_keyword())
        .collect();
    triggers.sort();
    if !triggers.is_empty() {
        parts.push(triggers.join(" "));
    }

    if let Some(proc) = &m.content.procedure {
        if let Some(first) = proc.steps.first() {
            parts.push(first.description.clone());
        }
    }
    parts.join("\n")
}
