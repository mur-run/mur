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

    if let Some(proc) = &m.content.procedure
        && let Some(first) = proc.steps.first()
    {
        parts.push(first.description.clone());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::manifest::{Content, Procedure, ProcedureStep, Trigger, Visibility};
    use mur_common::skill::types::TriggerKind;

    fn make_manifest(name: &str, desc: &str, abstract_: &str) -> SkillManifest {
        SkillManifest {
            name: name.into(),
            version: "1.0.0".into(),
            publisher: "test".into(),
            description: desc.into(),
            category: mur_common::skill::types::Category::Context,
            hosts: vec![],
            scope: Default::default(),
            visibility: Visibility::default(),
            origin: None,
            origin_version: None,
            origin_hash: None,
            fleet: None,
            project: None,
            team: None,
            governance: None,
            content: Content {
                r#abstract: abstract_.into(),
                context: Some("context text".into()),
                procedure: None,
                command: None,
                note: None,
            },
            requires: vec![],
            triggers: vec![],
            tags: vec![],
            priority: Default::default(),
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
            provenance: Default::default(),
            updated_at: chrono::Utc::now(),
            requires_programs: vec![],
        }
    }

    #[test]
    fn embed_manifest_basic_fields() {
        let m = make_manifest(
            "web-search",
            "Search the web",
            "Use this to find information online",
        );
        let text = embed_manifest(&m);
        assert!(
            text.starts_with("web-search\nSearch the web\nUse this to find information online")
        );
    }

    #[test]
    fn embed_manifest_includes_keyword_triggers_sorted() {
        let mut m = make_manifest("notify", "Send notifications", "Notify user");
        m.triggers = vec![
            Trigger {
                kind: TriggerKind::Keyword,
                pattern: Some("alert".into()),
            },
            Trigger {
                kind: TriggerKind::Command,
                pattern: Some("/run-cmd".into()),
            },
            Trigger {
                kind: TriggerKind::Keyword,
                pattern: Some("ping".into()),
            },
        ];
        let text = embed_manifest(&m);
        // Only Keyword triggers, sorted: alert, ping
        assert!(text.contains("alert ping"));
        // Command trigger excluded
        assert!(!text.contains("/run-cmd"));
    }

    #[test]
    fn embed_manifest_includes_first_procedure_step() {
        let mut m = make_manifest("deploy", "Deploy app", "Deployment workflow");
        m.content.procedure = Some(Procedure {
            variables: vec![],
            steps: vec![
                ProcedureStep {
                    description: "Check connectivity".into(),
                    tool: None,
                    intent: None,
                    tool_hint: None,
                    ..Default::default()
                },
                ProcedureStep {
                    description: "Push to server".into(),
                    tool: Some("rsync".into()),
                    intent: None,
                    tool_hint: None,
                    ..Default::default()
                },
            ],
        });
        let text = embed_manifest(&m);
        assert!(text.contains("Check connectivity"));
        assert!(!text.contains("Push to server"));
    }

    #[test]
    fn embed_manifest_no_triggers_no_procedure() {
        let m = make_manifest("minimal", "Minimal skill", "Just the basics");
        let text = embed_manifest(&m);
        // Three parts: name, description, abstract — no trigger or procedure lines.
        assert_eq!(text.matches('\n').count(), 2);
    }
}
