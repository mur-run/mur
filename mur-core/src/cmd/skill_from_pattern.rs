//! `mur skill from-pattern <name>` — promote a Stable/Canonical pattern to a skill.

use anyhow::{Context, Result, bail};
use mur_common::knowledge::Maturity;
use mur_common::pattern::Pattern;
use mur_common::skill::{Category, Content, HostId, SkillManifest, validate};

/// Layer 2 abstract budget. Pre-polish, the `principle` field is truncated to
/// this on a UTF-8 char boundary. With `--polish`, the LLM gets the full
/// principle and is asked to compress within the same budget.
const ABSTRACT_CHAR_BUDGET: usize = 250;

/// Extract (technical, principle) from a pattern's Content enum.
fn extract_content(content: &mur_common::pattern::Content) -> (String, String) {
    match content {
        mur_common::pattern::Content::DualLayer {
            technical,
            principle,
        } => (technical.clone(), principle.clone().unwrap_or_default()),
        mur_common::pattern::Content::Plain(s) => (s.clone(), String::new()),
    }
}

pub fn pattern_to_skill(pattern: &Pattern, polish: bool) -> Result<SkillManifest> {
    // Gate: only Stable or Canonical patterns are promoted.
    match pattern.maturity {
        Maturity::Stable | Maturity::Canonical => {}
        other => bail!(
            "pattern '{}' is {other:?} — only Stable or Canonical patterns can be promoted.\n\
             Use `mur pattern show {}` to see its current state.",
            pattern.name,
            pattern.name,
        ),
    }

    let (technical, principle) = extract_content(&pattern.content);

    // If the pattern has no principle (e.g. v1 Plain content), fall back to
    // truncating the description so the abstract is never empty.
    let source = if principle.is_empty() {
        &pattern.description
    } else {
        &principle
    };

    let abstract_text = if polish {
        // Polish path: leave full-length; LLM will rewrite + budget.
        source.clone()
    } else {
        // No-polish: char-safe truncation. `&str[..n]` would panic mid-codepoint
        // on Chinese/emoji content. ABSTRACT_CHAR_BUDGET counts chars, not bytes.
        let char_count = source.chars().count();
        if char_count > ABSTRACT_CHAR_BUDGET {
            let mut s: String = source.chars().take(ABSTRACT_CHAR_BUDGET).collect();
            s.push('…');
            s
        } else {
            source.clone()
        }
    };

    // Flatten Tags { languages, topics, extra } → Vec<String>.
    // De-dup preserves first occurrence so the manifest is stable across runs.
    let mut tags: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for t in pattern
        .tags
        .languages
        .iter()
        .chain(pattern.tags.topics.iter())
        .chain(pattern.tags.extra.values().flatten())
    {
        if seen.insert(t.clone()) {
            tags.push(t.clone());
        }
    }

    let manifest = SkillManifest {
        name: pattern.name.clone(),
        version: "0.1.0".into(),
        publisher: "agent:from-pattern".into(),
        description: pattern.description.clone(),
        category: Category::Context,
        hosts: vec![HostId::MurAgent],
        content: Content {
            r#abstract: abstract_text,
            context: Some(technical),
            procedure: None,
            command: None,
        },
        requires: vec![],
        tags,
        triggers: vec![],
        priority: Default::default(),
    };

    validate(&manifest).context("derived skill manifest failed schema validation")?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::{Content as PatternContent, Tags};
    use mur_common::knowledge::KnowledgeBase;

    fn make_pattern(name: &str, desc: &str, technical: &str, principle: &str, maturity: Maturity) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: desc.into(),
                content: PatternContent::DualLayer {
                    technical: technical.into(),
                    principle: Some(principle.into()),
                },
                maturity,
                tags: Tags::default(),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    fn make_draft_pattern() -> Pattern {
        make_pattern(
            "test-pattern",
            "A test pattern",
            "Run cargo test before pushing",
            "Always test before pushing to avoid broken builds",
            Maturity::Draft,
        )
    }

    fn make_stable(name: &str, desc: &str, technical: &str, principle: &str) -> Pattern {
        make_pattern(name, desc, technical, principle, Maturity::Stable)
    }

    fn make_canonical(name: &str) -> Pattern {
        make_pattern(name, "desc", "tech", "princ", Maturity::Canonical)
    }

    #[test]
    fn stable_pattern_produces_valid_manifest() {
        let p = make_stable("git-push-flow", "Always pull before push", "Run git pull --rebase before git push", "Prefer rebase over merge to keep history linear");
        let m = pattern_to_skill(&p, false).unwrap();
        validate(&m).unwrap();
        assert_eq!(m.name, "git-push-flow");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.publisher, "agent:from-pattern");
        assert!(m.content.context.is_some());
    }

    #[test]
    fn draft_pattern_rejected() {
        let p = make_draft_pattern();
        let err = pattern_to_skill(&p, false).unwrap_err();
        assert!(err.to_string().contains("Stable or Canonical"));
    }

    #[test]
    fn emerging_pattern_rejected() {
        let mut p = make_draft_pattern();
        p.maturity = Maturity::Emerging;
        let err = pattern_to_skill(&p, false).unwrap_err();
        assert!(err.to_string().contains("Stable or Canonical"));
    }

    #[test]
    fn canonical_pattern_accepted() {
        let p = make_canonical("canon-pattern");
        let m = pattern_to_skill(&p, false).unwrap();
        assert_eq!(m.name, "canon-pattern");
    }

    #[test]
    fn long_principle_truncated() {
        let long = "工".repeat(300);
        assert!(long.chars().count() == 300);
        let p = make_stable("trunc-test", "desc", "tech", &long);
        let m = pattern_to_skill(&p, false).unwrap();
        let abs = &m.content.r#abstract;
        // Budget 250 + '…' = 251 chars max
        assert!(abs.chars().count() <= 251, "got {} chars", abs.chars().count());
        assert!(abs.ends_with('…'));
    }

    #[test]
    fn utf8_boundary_chinese_no_panic() {
        let chinese = "工".repeat(300);
        let p = make_stable("utf8-test", "desc", "tech", &chinese);
        let m = pattern_to_skill(&p, false).unwrap();
        // Must produce valid UTF-8
        let _ = &m.content.r#abstract;
    }

    #[test]
    fn polish_preserves_full_principle() {
        let principle = "A".repeat(300);
        let p = make_stable("polish-test", "desc", "tech", &principle);
        let m = pattern_to_skill(&p, true).unwrap();
        // With polish=true, the full principle is preserved (LLM trims later)
        assert_eq!(m.content.r#abstract, principle);
    }

    #[test]
    fn tag_flattening() {
        let mut p = make_stable("tag-test", "desc", "tech", "princ");
        p.tags = Tags {
            languages: vec!["rust".into()],
            topics: vec!["cli".into(), "refactor".into()],
            extra: {
                let mut m = std::collections::HashMap::new();
                m.insert("project".into(), vec!["mur".into()]);
                m
            },
        };
        let m = pattern_to_skill(&p, false).unwrap();
        assert!(m.tags.contains(&"rust".to_string()));
        assert!(m.tags.contains(&"cli".to_string()));
        assert!(m.tags.contains(&"refactor".to_string()));
        assert!(m.tags.contains(&"mur".to_string()));
        // No duplicates
        let mut sorted = m.tags.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), m.tags.len());
    }

    #[test]
    fn invalid_name_surfaces_error() {
        let p = make_stable("bad name ", "desc", "tech", "princ");
        let err = pattern_to_skill(&p, false).unwrap_err();
        assert!(err.to_string().contains("validation"));
    }

    #[test]
    fn plain_content_v1_compat() {
        let p = Pattern {
            base: KnowledgeBase {
                name: "v1-legacy".into(),
                description: "v1 pattern".into(),
                content: PatternContent::Plain("just a string".into()),
                maturity: Maturity::Stable,
                tags: Tags::default(),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        };
        let m = pattern_to_skill(&p, false).unwrap();
        // Plain → principle empty → abstract falls back to description
        assert_eq!(m.content.r#abstract, "v1 pattern");
        assert_eq!(m.content.context.as_deref(), Some("just a string"));
    }
}
