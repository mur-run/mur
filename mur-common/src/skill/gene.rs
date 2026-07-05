//! Skill gene model (M7b).
//!
//! A `SkillGene` is a pure field-level projection of a `SkillManifest`. It is
//! not persisted — derived on demand. Two genes can be diffed and recombined
//! to produce a third manifest.
//!
//! Scope (M7b): procedure-mode skills only. Context-only or command-only
//! skills are not eligible — `from_manifest` returns `Err` for them.

use crate::skill::manifest::{Procedure, ProcedureStep, Requirement, SkillManifest, Trigger};
use crate::skill::mcp::{McpRequirement, SkillCapability};
use crate::skill::types::TriggerKind;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TriggerGene {
    pub kind: TriggerKind,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct McpGene {
    pub tool_pattern: String,
    pub capability: SkillCapability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepGene {
    /// `None` means the step is not matchable by intent in Intersection.
    pub intent: Option<String>,
    pub description: String,
    pub tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillGene {
    pub triggers: BTreeSet<TriggerGene>,
    pub steps: Vec<StepGene>,
    /// requirement name -> semver constraint string (as written in the
    /// manifest; parsed lazily by the Union semver merger).
    pub requires: BTreeMap<String, String>,
    pub mcp: BTreeSet<McpGene>,
}

#[derive(Debug, thiserror::Error)]
pub enum GeneError {
    #[error("skill is not procedure-mode (recombine requires procedure-mode skills)")]
    NotProcedure,
}

impl SkillGene {
    pub fn from_manifest(m: &SkillManifest) -> Result<Self, GeneError> {
        let proc = m
            .content
            .procedure
            .as_ref()
            .ok_or(GeneError::NotProcedure)?;

        let triggers = m
            .triggers
            .iter()
            .map(|t| TriggerGene {
                kind: t.kind,
                pattern: t.pattern.clone(),
            })
            .collect();

        let steps = proc
            .steps
            .iter()
            .map(|s| StepGene {
                intent: s.intent.clone(),
                description: s.description.clone(),
                tool: s.tool.clone(),
            })
            .collect();

        let requires = m
            .requires
            .iter()
            .map(|r| (r.name.clone(), r.version.clone()))
            .collect();

        let mcp = m
            .mcp_requirements
            .iter()
            .map(|r| McpGene {
                tool_pattern: r.tool_pattern.clone(),
                capability: r.capability,
            })
            .collect();

        Ok(SkillGene {
            triggers,
            steps,
            requires,
            mcp,
        })
    }

    /// Rebuild a `Procedure` from the steps in this gene (preserves order,
    /// no variables — Variables are copied from the keeper manifest by the
    /// orchestrator, not the gene layer).
    pub fn to_procedure(&self) -> Procedure {
        Procedure {
            variables: Vec::new(),
            steps: self
                .steps
                .iter()
                .map(|s| ProcedureStep {
                    description: s.description.clone(),
                    tool: s.tool.clone(),
                    intent: s.intent.clone(),
                    tool_hint: None,
                    ..Default::default()
                })
                .collect(),
        }
    }

    pub fn to_triggers(&self) -> Vec<Trigger> {
        self.triggers
            .iter()
            .map(|t| Trigger {
                kind: t.kind,
                pattern: t.pattern.clone(),
            })
            .collect()
    }

    pub fn to_requirements(&self) -> Vec<Requirement> {
        self.requires
            .iter()
            .map(|(name, version)| Requirement {
                name: name.clone(),
                version: version.clone(),
            })
            .collect()
    }

    pub fn to_mcp_requirements(&self) -> Vec<McpRequirement> {
        self.mcp
            .iter()
            .map(|g| McpRequirement {
                tool_pattern: g.tool_pattern.clone(),
                capability: g.capability,
                fallback: String::new(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GeneDiff {
    pub triggers_added: Vec<TriggerGene>,
    pub triggers_removed: Vec<TriggerGene>,
    pub steps_added: Vec<StepGene>,
    pub steps_removed: Vec<StepGene>,
    /// Same intent, different description or tool. (old, new).
    pub steps_changed: Vec<(StepGene, StepGene)>,
    /// (name, old_version, new_version).
    pub requires_changed: Vec<(String, String, String)>,
    pub requires_added: Vec<(String, String)>,
    pub requires_removed: Vec<(String, String)>,
    pub mcp_added: Vec<McpGene>,
    pub mcp_removed: Vec<McpGene>,
}

impl GeneDiff {
    pub fn between(a: &SkillGene, b: &SkillGene) -> Self {
        let mut d = GeneDiff {
            triggers_added: b.triggers.difference(&a.triggers).cloned().collect(),
            triggers_removed: a.triggers.difference(&b.triggers).cloned().collect(),
            mcp_added: b.mcp.difference(&a.mcp).cloned().collect(),
            mcp_removed: a.mcp.difference(&b.mcp).cloned().collect(),
            ..Default::default()
        };

        // Requires — key-wise
        for (name, a_ver) in &a.requires {
            match b.requires.get(name) {
                None => d.requires_removed.push((name.clone(), a_ver.clone())),
                Some(b_ver) if b_ver != a_ver => {
                    d.requires_changed
                        .push((name.clone(), a_ver.clone(), b_ver.clone()));
                }
                _ => {}
            }
        }
        for (name, b_ver) in &b.requires {
            if !a.requires.contains_key(name) {
                d.requires_added.push((name.clone(), b_ver.clone()));
            }
        }

        // Steps — match by intent (when both have Some(intent) and they match)
        let a_by_intent: BTreeMap<&str, &StepGene> = a
            .steps
            .iter()
            .filter_map(|s| s.intent.as_deref().map(|i| (i, s)))
            .collect();
        let b_by_intent: BTreeMap<&str, &StepGene> = b
            .steps
            .iter()
            .filter_map(|s| s.intent.as_deref().map(|i| (i, s)))
            .collect();

        for (intent, a_step) in &a_by_intent {
            match b_by_intent.get(intent) {
                None => d.steps_removed.push((*a_step).clone()),
                Some(b_step) if a_step != b_step => {
                    d.steps_changed.push(((*a_step).clone(), (*b_step).clone()));
                }
                _ => {}
            }
        }
        for (intent, b_step) in &b_by_intent {
            if !a_by_intent.contains_key(intent) {
                d.steps_added.push((*b_step).clone());
            }
        }

        // Steps without intent in either side are appended to added/removed
        // wholesale (they cannot be matched).
        for s in a.steps.iter().filter(|s| s.intent.is_none()) {
            d.steps_removed.push(s.clone());
        }
        for s in b.steps.iter().filter(|s| s.intent.is_none()) {
            d.steps_added.push(s.clone());
        }

        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::manifest::{Content, Procedure, ProcedureStep, Trigger, Visibility};
    use crate::skill::types::{Category, TriggerKind};

    fn manifest_with_steps(steps: Vec<ProcedureStep>, triggers: Vec<Trigger>) -> SkillManifest {
        SkillManifest {
            name: "x".into(),
            version: "0.1.0".into(),
            publisher: "human:test".into(),
            description: "t".into(),
            category: Category::Workflow,
            scope: Default::default(),
            visibility: Visibility::default(),
            origin: None,
            origin_version: None,
            origin_hash: None,
            fleet: None,
            team: None,
            governance: None,
            project: None,
            hosts: vec![],
            content: Content {
                r#abstract: "a".into(),
                context: None,
                procedure: Some(Procedure {
                    variables: vec![],
                    steps,
                }),
                command: None,
                note: None,
            },
            requires: vec![],
            tags: vec![],
            triggers,
            priority: Default::default(),
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
            provenance: Default::default(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn from_manifest_extracts_gene_fields() {
        let m = manifest_with_steps(
            vec![ProcedureStep {
                description: "navigate".into(),
                tool: Some("browser.go".into()),
                intent: Some("open_page".into()),
                tool_hint: None,
                ..Default::default()
            }],
            vec![Trigger {
                kind: TriggerKind::Command,
                pattern: Some("/x".into()),
            }],
        );
        let g = SkillGene::from_manifest(&m).unwrap();
        assert_eq!(g.steps.len(), 1);
        assert_eq!(g.steps[0].intent.as_deref(), Some("open_page"));
        assert_eq!(g.triggers.len(), 1);
    }

    #[test]
    fn from_manifest_rejects_non_procedure() {
        let mut m = manifest_with_steps(vec![], vec![]);
        m.content.procedure = None;
        m.content.context = Some("ctx".into());
        assert!(matches!(
            SkillGene::from_manifest(&m),
            Err(GeneError::NotProcedure)
        ));
    }

    #[test]
    fn diff_detects_added_and_changed_steps() {
        let a = manifest_with_steps(
            vec![ProcedureStep {
                description: "old".into(),
                tool: None,
                intent: Some("i1".into()),
                tool_hint: None,
                ..Default::default()
            }],
            vec![],
        );
        let b = manifest_with_steps(
            vec![
                ProcedureStep {
                    description: "new desc".into(),
                    tool: None,
                    intent: Some("i1".into()),
                    tool_hint: None,
                    ..Default::default()
                },
                ProcedureStep {
                    description: "added".into(),
                    tool: None,
                    intent: Some("i2".into()),
                    tool_hint: None,
                    ..Default::default()
                },
            ],
            vec![],
        );
        let ga = SkillGene::from_manifest(&a).unwrap();
        let gb = SkillGene::from_manifest(&b).unwrap();
        let d = GeneDiff::between(&ga, &gb);
        assert_eq!(d.steps_changed.len(), 1);
        assert_eq!(d.steps_added.len(), 1);
        assert_eq!(d.steps_removed.len(), 0);
    }

    #[test]
    fn diff_treats_intentless_steps_as_unmatched() {
        let a = manifest_with_steps(
            vec![ProcedureStep {
                description: "no-intent".into(),
                tool: None,
                intent: None,
                tool_hint: None,
                ..Default::default()
            }],
            vec![],
        );
        let b = manifest_with_steps(vec![], vec![]);
        let ga = SkillGene::from_manifest(&a).unwrap();
        let gb = SkillGene::from_manifest(&b).unwrap();
        let d = GeneDiff::between(&ga, &gb);
        assert_eq!(d.steps_removed.len(), 1);
    }

    #[test]
    fn round_trip_to_procedure_preserves_intent_and_tool() {
        let g = SkillGene {
            triggers: BTreeSet::new(),
            steps: vec![StepGene {
                intent: Some("i".into()),
                description: "d".into(),
                tool: Some("t".into()),
            }],
            requires: BTreeMap::new(),
            mcp: BTreeSet::new(),
        };
        let p = g.to_procedure();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].intent.as_deref(), Some("i"));
        assert_eq!(p.steps[0].tool.as_deref(), Some("t"));
        assert!(p.steps[0].tool_hint.is_none());
    }
}
