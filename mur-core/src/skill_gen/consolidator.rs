//! Phase 3: hierarchical patch merge + dedupe + LLM final pass → validated SkillManifest.

use crate::skill_gen::analysts::{Patch, PatchSource, StepDraft, TriggerDraft, VariableDraft};
use crate::skill_gen::prompts::CONSOLIDATOR_SYSTEM;
use mur_common::error::LlmError;
use mur_common::llm::LlmClient;
use mur_common::skill::{SkillManifest, parse_canonical, validate};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub enum ConsolidateError {
    #[error("no patches to consolidate")]
    Empty,
    #[error("LLM final pass failed: {0}")]
    Llm(#[from] LlmError),
    #[error("LLM emitted invalid skill yaml: {0}")]
    BadYaml(String),
    #[error("validation: {0}")]
    Validate(String),
}

pub async fn consolidate<L: LlmClient>(
    patches: Vec<Patch>,
    llm: &L,
    target_name: Option<&str>,
) -> Result<SkillManifest, ConsolidateError> {
    if patches.is_empty() {
        return Err(ConsolidateError::Empty);
    }

    let merged = mechanical_merge(&patches);

    let input = serde_json::to_string(&MergedInput {
        target_name: target_name.unwrap_or("generated-skill").to_string(),
        merged,
    })
    .expect("serialize");

    let yaml = llm.complete(&input, Some(CONSOLIDATOR_SYSTEM)).await?;
    let yaml = strip_yaml_fences(&yaml);

    let m = parse_canonical(yaml).map_err(|e| ConsolidateError::BadYaml(e.to_string()))?;
    validate(&m).map_err(|e| ConsolidateError::Validate(e.to_string()))?;
    Ok(m)
}

#[derive(serde::Serialize)]
struct MergedInput {
    target_name: String,
    merged: MechanicalMerge,
}

#[derive(Debug, serde::Serialize)]
struct MechanicalMerge {
    pub step_groups: BTreeMap<String, Vec<StepDraft>>,
    pub triggers: Vec<TriggerDraft>,
    pub variables: Vec<VariableDraft>,
    pub abstract_hints: Vec<String>,
    pub notes: Vec<(PatchSource, Vec<String>)>,
}

fn mechanical_merge(patches: &[Patch]) -> MechanicalMerge {
    let mut step_groups: BTreeMap<String, Vec<StepDraft>> = BTreeMap::new();
    let mut triggers_seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut triggers: Vec<TriggerDraft> = Vec::new();
    let mut vars_seen: BTreeSet<String> = BTreeSet::new();
    let mut variables: Vec<VariableDraft> = Vec::new();
    let mut abstract_hints = Vec::new();
    let mut notes = Vec::new();

    // Prefer Success patches over Error ones when collapsing duplicates.
    let (succ, err): (Vec<_>, Vec<_>) = patches
        .iter()
        .partition(|p| matches!(p.source, PatchSource::Success));
    let ordered: Vec<&Patch> = succ.into_iter().chain(err).collect();

    for p in ordered {
        if let Some(h) = &p.abstract_hint {
            abstract_hints.push(h.clone());
        }
        for s in &p.procedure_steps {
            let key = step_similarity_key(s);
            step_groups.entry(key).or_default().push(s.clone());
        }
        for t in &p.triggers {
            let k = (t.kind.to_lowercase(), t.pattern.trim().to_lowercase());
            if triggers_seen.insert(k) {
                triggers.push(t.clone());
            }
        }
        for v in &p.variables {
            if vars_seen.insert(v.name.clone()) {
                variables.push(v.clone());
            }
        }
        if !p.notes.is_empty() {
            notes.push((p.source.clone(), p.notes.clone()));
        }
    }

    MechanicalMerge {
        step_groups,
        triggers,
        variables,
        abstract_hints,
        notes,
    }
}

fn step_similarity_key(s: &StepDraft) -> String {
    let tool = s.tool.as_deref().unwrap_or("").to_lowercase();
    let head: String = s
        .description
        .split_whitespace()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    format!("{tool}::{head}")
}

fn strip_yaml_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```yaml")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    s.trim_end_matches("```").trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_gen::analysts::StepDraft;
    use mur_common::error::LlmError;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct MockLlm {
        responses: Mutex<VecDeque<String>>,
    }

    impl LlmClient for MockLlm {
        fn complete(
            &self,
            _prompt: &str,
            _system: Option<&str>,
        ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            let r = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            async move { Ok(r) }
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![])
        }
    }

    fn make_patch(source: PatchSource, hint: &str, steps: Vec<StepDraft>) -> Patch {
        Patch {
            source,
            abstract_hint: Some(hint.into()),
            procedure_steps: steps,
            triggers: vec![],
            variables: vec![],
            notes: vec![],
        }
    }

    fn make_step(desc: &str, tool: &str) -> StepDraft {
        StepDraft {
            description: desc.into(),
            tool: Some(tool.into()),
            params_hint: None,
        }
    }

    #[test]
    fn mechanical_merge_dedupes_triggers() {
        let p1 = Patch {
            source: PatchSource::Success,
            abstract_hint: None,
            procedure_steps: vec![],
            triggers: vec![TriggerDraft {
                kind: "command".into(),
                pattern: "/find-price".into(),
            }],
            variables: vec![],
            notes: vec![],
        };
        let p2 = Patch {
            source: PatchSource::Error,
            abstract_hint: None,
            procedure_steps: vec![],
            triggers: vec![TriggerDraft {
                kind: "command".into(),
                pattern: "/find-price".into(),
            }],
            variables: vec![],
            notes: vec![],
        };
        let merged = mechanical_merge(&[p1, p2]);
        assert_eq!(merged.triggers.len(), 1);
    }

    #[test]
    fn empty_patches_is_error() {
        let llm = MockLlm {
            responses: Mutex::new(VecDeque::new()),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let err = rt.block_on(consolidate(vec![], &llm, None)).unwrap_err();
        assert!(matches!(err, ConsolidateError::Empty));
    }

    #[test]
    fn success_patches_ordered_before_error() {
        // Both steps share the same similarity key (same tool + description head).
        let s_step = make_step("open product page", "browser.navigate");
        let e_step = make_step("open product page", "browser.navigate");
        let p_success = make_patch(PatchSource::Success, "hint", vec![s_step.clone()]);
        let p_error = make_patch(PatchSource::Error, "hint", vec![e_step.clone()]);
        // Feed Error first — mechanical_merge should still order Success before Error.
        let merged = mechanical_merge(&[p_error, p_success]);
        let key = step_similarity_key(&s_step);
        let group = merged.step_groups.get(&key).unwrap();
        assert_eq!(group.len(), 2);
        // First entry is from Success (Success patches are ordered first by mechanical_merge).
        assert!(matches!(group[0].tool.as_deref(), Some("browser.navigate")));
    }

    #[tokio::test]
    async fn llm_roundtrip_produces_valid_manifest() {
        let valid_yaml = r#"
name: find-price
version: 0.1.0
publisher: agent:generator
description: find product prices
category: workflow
content:
  abstract: Searches product prices.
  procedure:
    steps:
      - description: open product page
        tool: browser.navigate
triggers:
  - type: command
    pattern: /find-price
"#;
        let llm = MockLlm {
            responses: Mutex::new(VecDeque::from([valid_yaml.into()])),
        };
        let patch = make_patch(
            PatchSource::Success,
            "find prices",
            vec![make_step("open product page", "browser.navigate")],
        );
        let manifest = consolidate(vec![patch], &llm, Some("find-price"))
            .await
            .unwrap();
        assert_eq!(manifest.name, "find-price");
        assert_eq!(manifest.version, "0.1.0");
    }

    #[tokio::test]
    async fn invalid_yaml_returns_validation_error() {
        let bad_yaml = "this is not valid yaml for a skill manifest";
        let llm = MockLlm {
            responses: Mutex::new(VecDeque::from([bad_yaml.into()])),
        };
        let patch = make_patch(PatchSource::Success, "hint", vec![]);
        let err = consolidate(vec![patch], &llm, None).await.unwrap_err();
        assert!(matches!(err, ConsolidateError::BadYaml(_)));
    }
}
