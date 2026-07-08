use mur_common::skill::TriggerKind;
use mur_common::skill::loader::LoadedSkill;
use mur_common::skill::manifest::ProcedureStep;
use mur_common::skill::types::TrustLevel;
use mur_common::skill::{McpInventory, Resolution};
use regex::Regex;

#[derive(Debug, Clone)]
pub struct RegisteredTrigger {
    pub skill_name: String,
    pub pattern: TriggerPattern,
    pub trust: TrustLevel,
}

#[derive(Debug, Clone)]
pub enum TriggerPattern {
    Command(String),
    Keyword(Regex),
}

pub fn register_from(skills: &[LoadedSkill]) -> Vec<RegisteredTrigger> {
    let mut out = Vec::new();
    for s in skills {
        for t in &s.manifest.triggers {
            let p_opt = match (&t.kind, &t.pattern) {
                (TriggerKind::Command, Some(p)) => Some(TriggerPattern::Command(p.clone())),
                (TriggerKind::Keyword, Some(p)) => match Regex::new(p) {
                    Ok(rx) => Some(TriggerPattern::Keyword(rx)),
                    Err(e) => {
                        tracing::warn!(
                            skill = %s.name,
                            pattern = %p,
                            error = %e,
                            "bad keyword regex"
                        );
                        None
                    }
                },
                _ => None,
            };
            if let Some(pattern) = p_opt {
                out.push(RegisteredTrigger {
                    skill_name: s.name.clone(),
                    pattern,
                    trust: s.trust,
                });
            }
        }
    }
    out
}

pub fn match_prompt<'a>(
    triggers: &'a [RegisteredTrigger],
    prompt: &str,
) -> Vec<&'a RegisteredTrigger> {
    triggers
        .iter()
        .filter(|t| match &t.pattern {
            TriggerPattern::Command(cmd) => prompt.trim_start().starts_with(cmd),
            TriggerPattern::Keyword(rx) => rx.is_match(prompt),
        })
        .collect()
}

pub fn layer3_body(
    manifest: &mur_common::skill::SkillManifest,
    inventory: &McpInventory,
) -> Option<String> {
    let c = &manifest.content;
    if let Some(ctx) = &c.context {
        return Some(ctx.clone());
    }
    if let Some(p) = &c.procedure {
        return Some(
            p.steps
                .iter()
                .enumerate()
                .map(|(i, step)| {
                    let res = mur_common::skill::resolve_step(
                        step,
                        &manifest.mcp_requirements,
                        inventory,
                    );
                    render_step(i + 1, step, &res)
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    c.command.clone()
}

fn render_step(idx: usize, step: &ProcedureStep, res: &Resolution) -> String {
    match res.picked_tool() {
        Some(tool) => format!("{idx}. {} — tool: {tool}", step.description),
        None => format!(
            "{idx}. {} — (no tool available: {})",
            step.description,
            match res {
                Resolution::Unresolved { reason } => reason.as_str(),
                _ => "unknown",
            }
        ),
    }
}

/// Escape a string for safe inclusion in an XML attribute value (double-quoted).
/// Prevents a skill `name` or `trust` containing `"` or `>` from breaking
/// the attribute boundary and injecting arbitrary markup.
fn xml_attr_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

pub fn format_layer3(skill_name: &str, trust: TrustLevel, body: &str) -> String {
    let safe_name = xml_attr_escape(skill_name);
    let safe_trust = xml_attr_escape(&format!("{trust:?}"));
    format!(
        "<skill-instruction source=\"{safe_name}\" trust=\"{safe_trust}\">\n{body}\n</skill-instruction>"
    )
}

/// If the skill's install directory holds a real bundled file (anything besides
/// `skill.yaml` and hidden dotfiles like `.DS_Store`), return a one-line hint
/// telling the agent where the bundle lives so paths like
/// `scripts/start-server.sh` resolve. Returns None for asset-free skills (and,
/// fail-safe, when the directory can't be read).
pub fn bundle_hint(dir: &std::path::Path) -> Option<String> {
    let mut has_bundle = false;
    for entry in std::fs::read_dir(dir).ok()? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        // Ignore the manifest and OS/editor junk (`.DS_Store`, swap files, …),
        // which are dotfiles, not shipped bundle content.
        if name == "skill.yaml" || name.to_string_lossy().starts_with('.') {
            continue;
        }
        has_bundle = true;
        break;
    }
    if !has_bundle {
        return None;
    }
    Some(format!(
        "\n\nBundled files for this skill are on disk at: {0}\n\
         (e.g. run a script with the bash tool: `bash {0}/scripts/<file>`)",
        dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::loader::SkillScope;
    use mur_common::skill::parse_canonical;

    #[test]
    fn bundle_hint_present_when_extra_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("skill.yaml"), "x").unwrap();
        std::fs::create_dir_all(tmp.path().join("scripts")).unwrap();
        let hint = bundle_hint(tmp.path()).unwrap();
        assert!(hint.contains(&tmp.path().display().to_string()));
        assert!(hint.contains("scripts"));
    }

    #[test]
    fn bundle_hint_absent_when_only_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("skill.yaml"), "x").unwrap();
        assert!(bundle_hint(tmp.path()).is_none());
    }

    #[test]
    fn bundle_hint_ignores_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("skill.yaml"), "x").unwrap();
        std::fs::write(tmp.path().join(".DS_Store"), "junk").unwrap();
        assert!(
            bundle_hint(tmp.path()).is_none(),
            "a stray .DS_Store must not count as a bundle"
        );
    }

    fn sample() -> LoadedSkill {
        let yaml = r#"
name: research
version: 1.0.0
publisher: human:t
description: r
category: context
content:
  abstract: Searches prices
  context: "Full procedure: navigate, search, extract"
triggers:
  - type: command
    pattern: /research
  - type: keyword
    pattern: "find prices"
"#;
        let m = parse_canonical(yaml).unwrap();
        LoadedSkill {
            name: "research".into(),
            manifest: m,
            trust: TrustLevel::Verified,
            scope: SkillScope::Global,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn command_trigger_matches() {
        let triggers = register_from(&[sample()]);
        let matched = match_prompt(&triggers, "/research airpods");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].skill_name, "research");
    }

    #[test]
    fn keyword_trigger_matches() {
        let triggers = register_from(&[sample()]);
        let matched = match_prompt(&triggers, "can you find prices please?");
        assert_eq!(matched.len(), 1);
    }

    #[test]
    fn invalid_regex_is_skipped() {
        let yaml = r#"
name: bad-regex
version: 1.0.0
publisher: human:t
description: r
category: context
content:
  abstract: x
  context: x
triggers:
  - type: keyword
    pattern: "(unmatched["
"#;
        let m = parse_canonical(yaml).unwrap();
        let s = LoadedSkill {
            name: "bad-regex".into(),
            manifest: m,
            trust: TrustLevel::Sandboxed,
            scope: SkillScope::Global,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        };
        let triggers = register_from(&[s]);
        assert!(triggers.is_empty());
    }

    #[test]
    fn format_layer3_produces_tag() {
        let out = format_layer3("test", TrustLevel::Sandboxed, "do something");
        assert!(out.starts_with("<skill-instruction source=\"test\" trust=\"Sandboxed\">"));
        assert!(out.contains("do something"));
        assert!(out.ends_with("</skill-instruction>"));
    }

    #[test]
    fn layer3_body_extracts_context() {
        let body = layer3_body(&sample().manifest, &McpInventory::default());
        assert!(body.is_some());
        assert!(body.unwrap().contains("Full procedure"));
    }

    #[test]
    fn layer3_body_renders_literal_tool_step() {
        use mur_common::skill::parse_canonical;
        let yaml = r#"
name: test-skill
version: 1.0.0
publisher: human:t
description: test
category: workflow
content:
  abstract: test
  procedure:
    steps:
      - description: Navigate to page
        tool: browser.navigate
"#;
        let m = parse_canonical(yaml).unwrap();
        let body = layer3_body(&m, &McpInventory::default()).unwrap();
        assert!(body.contains("Navigate to page — tool: browser.navigate"));
    }
}
