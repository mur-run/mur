//! Shared construction for `Category::Note` skills — ONE manifest literal
//! instead of three drifting copies (`mur notes create`, the runtime's
//! `remember` tool, and the TUI `/remember` command all build the same note).

use chrono::Utc;

use crate::skill::lifecycle::NoteKind;
use crate::skill::manifest::{Content, SkillManifest, Visibility};
use crate::skill::types::{Category, Priority};

/// Inputs that vary between note authors; everything else is fixed note shape.
pub struct NoteSpec<'a> {
    pub name: &'a str,
    /// One-line summary (also becomes `content.abstract`).
    pub description: &'a str,
    /// Markdown body (`content.note`).
    pub body: &'a str,
    pub kind: NoteKind,
    /// Author identity, e.g. `human:local` or `agent:<name>`.
    pub publisher: &'a str,
}

/// Build the canonical note manifest. Callers still run
/// `crate::skill::validate` and choose WHERE to write (global vs agent-local)
/// — scope is the caller's decision, shape is not.
pub fn note_manifest(spec: &NoteSpec<'_>) -> SkillManifest {
    SkillManifest {
        name: spec.name.to_string(),
        version: "1.0.0".into(),
        publisher: spec.publisher.to_string(),
        description: spec.description.to_string(),
        category: Category::Note,
        hosts: vec![],
        scope: Default::default(),
        visibility: Visibility::default(),
        origin: None,
        origin_version: None,
        origin_hash: None,
        fleet: None,
        team: None,
        governance: None,
        project: None,
        content: Content {
            r#abstract: spec.description.to_string(),
            context: None,
            procedure: None,
            command: None,
            note: Some(spec.body.to_string()),
        },
        requires: vec![],
        // Kind lives in the tags: a `rule` tag marks a rule; a plain note is a
        // fact. `lifecycle::note_kind()` is the single reader.
        tags: match spec.kind {
            NoteKind::Rule => vec!["rule".into()],
            NoteKind::Fact => vec![],
        },
        triggers: vec![],
        priority: Priority::Normal,
        evolution_log: vec![],
        transfer_chain: vec![],
        mcp_requirements: vec![],
        provenance: Default::default(),
        updated_at: Utc::now(),
        requires_programs: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_manifest_validates_and_roundtrips_kind() {
        for (kind, expect) in [
            (NoteKind::Rule, Some(NoteKind::Rule)),
            (NoteKind::Fact, Some(NoteKind::Fact)),
        ] {
            let m = note_manifest(&NoteSpec {
                name: "t-note",
                description: "d",
                body: "b",
                kind,
                publisher: "human:t",
            });
            crate::skill::validate(&m).expect("canonical note must validate");
            assert_eq!(crate::skill::lifecycle::note_kind(&m), expect);
        }
    }
}
