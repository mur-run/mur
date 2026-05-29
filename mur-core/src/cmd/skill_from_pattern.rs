//! `mur skill from-pattern <name>` — promote a Stable/Canonical pattern to a skill.

use anyhow::{Context, Result, bail};
use mur_common::knowledge::Maturity;
use mur_common::pattern::Pattern;
use mur_common::skill::{
    Category, Content, HostId, Procedure, ProcedureStep, SkillManifest, Trigger, TriggerKind,
    validate,
};

use crate::conversations::backend::{ChatBackend, ChatRequest};

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
            note: None,
        },
        requires: vec![],
        tags,
        triggers: vec![],
        priority: Default::default(),
        evolution_log: vec![],
        transfer_chain: vec![],
        mcp_requirements: vec![],
    };

    validate(&manifest).context("derived skill manifest failed schema validation")?;
    Ok(manifest)
}

const POLISH_MAX_TOKENS: u32 = 1024;
const POLISH_MODEL: &str = "claude-sonnet-4-6";

const POLISH_SYSTEM: &str = r#"
You are a skill editor. Given a pattern extracted from agent behavior,
produce a polished skill.yaml abstract and suggest procedure steps.

The pattern's `technical` content describes what the agent actually did.
The `principle` describes the higher-level rule.

Output JSON only — no markdown fences, no prose:
{
  "abstract": "one-line polished abstract (≤200 chars)",
  "procedure_steps": [
    {"description": "step 1", "tool": "optional.tool.name"}
  ],
  "triggers": [{"type": "command", "pattern": "/suggested-trigger"}]
}

Rules:
- The abstract must capture the pattern's principle at a higher level.
- Procedure steps are derived from the technical content.
- If the technical content is too vague, suggest at most 2 steps.
"#;

pub async fn polish_via_llm(
    manifest: &mut SkillManifest,
    pattern: &Pattern,
    backend: &dyn ChatBackend,
) -> Result<()> {
    let (technical, principle) = extract_content(&pattern.content);

    let user = format!(
        "Pattern name: {}\nDescription: {}\nPrinciple: {}\nTechnical: {}",
        pattern.name, pattern.description, principle, technical,
    );

    let req = ChatRequest {
        model: POLISH_MODEL,
        system: Some(POLISH_SYSTEM),
        user: &user,
        max_tokens: POLISH_MAX_TOKENS,
        temperature: Some(0.2),
        stop: vec![],
        cache_system: true,
        cache_user_prefix: None,
    };
    let resp = backend
        .generate(req)
        .await
        .context("polish LLM call failed")?;

    // Defensive: some backends still wrap JSON in fences even when told not to.
    let raw = resp.text.trim();
    let raw = raw
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let v: serde_json::Value = serde_json::from_str(raw)
        .with_context(|| format!("LLM did not return valid JSON; got: {raw:.200}"))?;

    if let Some(a) = v.get("abstract").and_then(|x| x.as_str()) {
        manifest.content.r#abstract = a.to_string();
    }
    if let Some(steps) = v.get("procedure_steps").and_then(|x| x.as_array()) {
        let steps: Vec<ProcedureStep> = steps
            .iter()
            .filter_map(|s| {
                Some(ProcedureStep {
                    description: s.get("description")?.as_str()?.to_string(),
                    tool: s.get("tool").and_then(|x| x.as_str()).map(String::from),
                    intent: None,
                    tool_hint: None,
                })
            })
            .collect();
        if !steps.is_empty() {
            manifest.content.procedure = Some(Procedure {
                variables: vec![],
                steps,
            });
        }
    }
    if let Some(triggers) = v.get("triggers").and_then(|x| x.as_array()) {
        manifest.triggers = triggers
            .iter()
            .filter_map(|t| {
                let kind = match t.get("type")?.as_str()? {
                    "command" => TriggerKind::Command,
                    "keyword" => TriggerKind::Keyword,
                    _ => return None,
                };
                Some(Trigger {
                    kind,
                    pattern: t.get("pattern").and_then(|x| x.as_str()).map(String::from),
                })
            })
            .collect();
    }
    Ok(())
}

/// Test- and lib-friendly entry point. All I/O is scoped under `mur_home`.
///
/// When `polish=true` this function will make an LLM call (Task 3).
/// Kept `async` so the CLI shim can `.await` it directly without spinning a
/// nested runtime — `dispatch::run` is already async.
pub async fn cmd_from_pattern_with_home(
    mur_home: &std::path::Path,
    name: &str,
    polish: bool,
) -> Result<()> {
    use crate::store::yaml::YamlStore;
    use mur_common::skill::{
        TrustLevel, content_sha256, global_skill_dir, scan::scan_skill, write_to_dir,
    };
    use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

    let patterns_dir = mur_home.join("patterns");
    let store = YamlStore::new(patterns_dir).context("open pattern store")?;
    let pattern = store.get(name).with_context(|| {
        format!(
            "pattern '{name}' not found in {}/patterns/",
            mur_home.display()
        )
    })?;

    let mut manifest = pattern_to_skill(&pattern, polish)?;

    if polish {
        use crate::conversations::backend::factory;
        use mur_common::config::Config;

        let cfg_path = mur_home.join("config.yaml");
        let cfg = Config::load_or_default(&cfg_path);
        let backend = factory::build_for_stage(
            &cfg.conversations.ask.synthesize_backend(),
            "skill.from-pattern",
        )
        .context("build LLM backend for polish")?;
        polish_via_llm(&mut manifest, &pattern, backend.as_ref())
            .await
            .context("polish failed — re-run without --polish to install the unpolished skill")?;
        // Polish mutated the manifest — re-validate before write.
        mur_common::skill::validate(&manifest)
            .context("polished skill manifest failed schema validation")?;
    }

    // Scan the (possibly-polished) manifest. Pattern-promoted skills always
    // land at Sandboxed regardless of findings, but findings still get
    // surfaced so the user can decide whether to `mur skill trust` them up.
    let report = scan_skill(&manifest).context("scan derived skill manifest")?;
    if report.has_blocking_findings() {
        let n = report.human_summary().len();
        eprintln!(
            "warning: derived skill has {n} blocking content finding(s) — staying Sandboxed.\n\
             Run `mur skill audit {}` after install for details.",
            manifest.name,
        );
    }

    let dir = global_skill_dir(mur_home, &manifest.name);
    write_to_dir(&dir, &manifest).context("write skill manifest")?;

    let hash = content_sha256(&manifest).context("hash skill manifest")?;
    let mut trust =
        SkillTrustStore::load(mur_home).map_err(|e| anyhow::anyhow!("load trust store: {e}"))?;
    trust.insert(
        hash,
        TrustEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            level: TrustLevel::Sandboxed,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(manifest.publisher.clone()),
        },
    );
    trust
        .save(mur_home)
        .map_err(|e| anyhow::anyhow!("save trust store: {e}"))?;

    println!(
        "promoted: {} v{} (Sandboxed, from {:?})",
        manifest.name, manifest.version, pattern.maturity
    );
    if !polish {
        println!("hint: re-run with --polish for LLM-assisted abstract + procedure generation");
    }

    // M7c: append Author entry to credit ledger.
    if let Ok(Some(caller)) = crate::cmd::skill_install::caller_agent_name(mur_home) {
        let entry = mur_common::skill::credit::CreditEntry {
            ts: chrono::Utc::now(),
            skill: manifest.name.clone(),
            skill_version: manifest.version.clone(),
            kind: mur_common::skill::credit::CreditKind::Author,
            agent: caller.clone(),
            evidence: None,
            source: format!("human:{caller}"),
        };
        if let Err(e) = crate::cross_agent::credit::ledger::append(mur_home, &caller, &entry) {
            tracing::warn!("credit ledger append failed at from-pattern: {e}");
        }
    }

    Ok(())
}

/// Production entry point. Thin shim — resolves `~/.mur` and delegates.
pub async fn cmd_from_pattern(name: &str, polish: bool) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    cmd_from_pattern_with_home(&home, name, polish).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::{Content as PatternContent, Tags};

    fn make_pattern(
        name: &str,
        desc: &str,
        technical: &str,
        principle: &str,
        maturity: Maturity,
    ) -> Pattern {
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
        let p = make_stable(
            "git-push-flow",
            "Always pull before push",
            "Run git pull --rebase before git push",
            "Prefer rebase over merge to keep history linear",
        );
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
        assert!(
            abs.chars().count() <= 251,
            "got {} chars",
            abs.chars().count()
        );
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

    // --- Integration tests (drive cmd_from_pattern_with_home) ---

    use crate::store::yaml::YamlStore;
    use mur_common::skill::{TrustLevel, read_from_dir, validate};
    use mur_common::trust::skills::SkillTrustStore;

    #[tokio::test]
    async fn promotes_stable_pattern_to_sandboxed_skill() {
        let home = tempfile::tempdir().unwrap();

        // 1. Create a Stable pattern in tempdir.
        let store = YamlStore::new(home.path().join("patterns")).unwrap();
        let p = make_stable(
            "git-push-flow",
            "Always pull before push",
            "Run git pull --rebase before git push",
            "Prefer rebase over merge",
        );
        store.save(&p).unwrap();

        // 2. Run from-pattern (polish=false → no LLM call, no network).
        cmd_from_pattern_with_home(home.path(), "git-push-flow", false)
            .await
            .unwrap();

        // 3. Assert skill.yaml exists and validates.
        let skill_dir = home.path().join("skills/git-push-flow");
        assert!(skill_dir.join("skill.yaml").exists());
        let m = read_from_dir(&skill_dir).unwrap();
        validate(&m).unwrap();
        assert_eq!(m.publisher, "agent:from-pattern");
        assert_eq!(m.version, "0.1.0");

        // 4. Trust entry is Sandboxed and tagged with the right publisher.
        let trust = SkillTrustStore::load(home.path()).unwrap();
        let entry = trust
            .entries
            .values()
            .find(|e| e.name == "git-push-flow")
            .unwrap();
        assert!(matches!(entry.level, TrustLevel::Sandboxed));
        assert_eq!(entry.publisher.as_deref(), Some("agent:from-pattern"));
    }

    #[tokio::test]
    async fn rejects_draft_pattern() {
        let home = tempfile::tempdir().unwrap();
        let store = YamlStore::new(home.path().join("patterns")).unwrap();
        let p = make_draft_pattern();
        store.save(&p).unwrap();

        let err = cmd_from_pattern_with_home(home.path(), "test-pattern", false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Stable or Canonical"));
    }
}
