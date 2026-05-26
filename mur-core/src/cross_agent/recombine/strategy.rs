//! Recombination strategies.

use mur_common::skill::constraint::{Constraint, ConstraintError};
use mur_common::skill::gene::SkillGene;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecombineStrategy {
    Union,
    Intersection,
    Llm,
}

impl RecombineStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecombineStrategy::Union => "union",
            RecombineStrategy::Intersection => "intersection",
            RecombineStrategy::Llm => "llm",
        }
    }
}

/// Tiebreak inputs for Intersection's per-step keeper selection.
#[derive(Debug, Clone)]
pub struct FitnessCtx {
    pub a_agent: String,
    pub b_agent: String,
    pub a_success_rate: f64,
    pub b_success_rate: f64,
    pub a_weight: f64,
    pub b_weight: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum StrategyError {
    #[error("disjoint semver constraints for '{name}': '{a}' AND '{b}' have no overlap")]
    DisjointSemver { name: String, a: String, b: String },
    #[error("invalid semver constraint for '{name}': '{value}' ({source})")]
    InvalidSemver {
        name: String,
        value: String,
        source: ConstraintError,
    },
    #[error("intersection produced empty {what}; try --strategy=union")]
    EmptyIntersection { what: &'static str },
}

/// Union strategy — superset of both parents.
pub fn union(a: &SkillGene, b: &SkillGene) -> Result<SkillGene, StrategyError> {
    // Triggers + MCP: set union
    let mut triggers = a.triggers.clone();
    triggers.extend(b.triggers.iter().cloned());

    let mut mcp = a.mcp.clone();
    mcp.extend(b.mcp.iter().cloned());

    // Requires: merge per key with strict semver intersection
    let mut requires: BTreeMap<String, String> = a.requires.clone();
    for (name, b_ver) in &b.requires {
        match requires.get(name).cloned() {
            None => {
                requires.insert(name.clone(), b_ver.clone());
            }
            Some(a_ver) if a_ver == *b_ver => { /* identical, keep */ }
            Some(a_ver) => {
                let merged = merge_semver(name, &a_ver, b_ver)?;
                requires.insert(name.clone(), merged);
            }
        }
    }

    // Steps: round-robin interleave
    let mut steps = Vec::with_capacity(a.steps.len() + b.steps.len());
    let max_len = a.steps.len().max(b.steps.len());
    for i in 0..max_len {
        if let Some(s) = a.steps.get(i) {
            steps.push(s.clone());
        }
        if let Some(s) = b.steps.get(i) {
            steps.push(s.clone());
        }
    }

    Ok(SkillGene {
        triggers,
        steps,
        requires,
        mcp,
    })
}

/// Intersection strategy — only what both parents share.
pub fn intersection(
    a: &SkillGene,
    b: &SkillGene,
    fit: &FitnessCtx,
) -> Result<SkillGene, StrategyError> {
    // Triggers + MCP — set intersection
    let triggers: BTreeSet<_> = a.triggers.intersection(&b.triggers).cloned().collect();
    if triggers.is_empty() {
        return Err(StrategyError::EmptyIntersection { what: "triggers" });
    }
    let mcp: BTreeSet<_> = a.mcp.intersection(&b.mcp).cloned().collect();

    // Requires — keys ∩, then strict semver merge
    let mut requires: BTreeMap<String, String> = BTreeMap::new();
    for (name, a_ver) in &a.requires {
        if let Some(b_ver) = b.requires.get(name) {
            let merged = if a_ver == b_ver {
                a_ver.clone()
            } else {
                merge_semver(name, a_ver, b_ver)?
            };
            requires.insert(name.clone(), merged);
        }
    }

    // Steps — match by intent (Some-only), pick keeper per fitness rules
    let a_by_intent: BTreeMap<&str, &mur_common::skill::gene::StepGene> = a
        .steps
        .iter()
        .filter_map(|s| s.intent.as_deref().map(|i| (i, s)))
        .collect();
    let b_by_intent: BTreeMap<&str, &mur_common::skill::gene::StepGene> = b
        .steps
        .iter()
        .filter_map(|s| s.intent.as_deref().map(|i| (i, s)))
        .collect();

    let mut shared_intents: Vec<&str> = a_by_intent
        .keys()
        .filter(|k| b_by_intent.contains_key(*k))
        .copied()
        .collect();
    shared_intents.sort(); // deterministic order

    if shared_intents.is_empty() {
        return Err(StrategyError::EmptyIntersection { what: "steps" });
    }

    let prefer_a = pick_a_over_b(fit);
    let mut steps = Vec::with_capacity(shared_intents.len());
    for intent in shared_intents {
        let pick = if prefer_a {
            a_by_intent[intent]
        } else {
            b_by_intent[intent]
        };
        steps.push(pick.clone());
    }

    Ok(SkillGene {
        triggers,
        steps,
        requires,
        mcp,
    })
}

/// Combine two semver constraint strings into the strictest constraint that
/// satisfies both. Returns `Err(DisjointSemver)` when no version satisfies
/// both inputs.
pub fn merge_semver(name: &str, a: &str, b: &str) -> Result<String, StrategyError> {
    let ca = Constraint::parse(a).map_err(|e| StrategyError::InvalidSemver {
        name: name.to_string(),
        value: a.to_string(),
        source: e,
    })?;
    let cb = Constraint::parse(b).map_err(|e| StrategyError::InvalidSemver {
        name: name.to_string(),
        value: b.to_string(),
        source: e,
    })?;

    let combined_str = format!("{a},{b}");
    let combined: VersionReq = combined_str.parse().map_err(|_| StrategyError::DisjointSemver {
        name: name.to_string(),
        a: a.to_string(),
        b: b.to_string(),
    })?;

    if !has_any_satisfying_version(&combined, &ca, &cb) {
        return Err(StrategyError::DisjointSemver {
            name: name.to_string(),
            a: a.to_string(),
            b: b.to_string(),
        });
    }

    Ok(combined_str)
}

/// Probe a small set of versions to confirm the merged constraint is
/// satisfiable. Not exhaustive — catches the common "disjoint upper/lower"
/// failure mode.
fn has_any_satisfying_version(req: &VersionReq, ca: &Constraint, cb: &Constraint) -> bool {
    for major in 0..100u64 {
        for minor in [0u64, 1, 5, 10] {
            for patch in [0u64, 1, 5] {
                let v = Version::new(major, minor, patch);
                if req.matches(&v) && ca.matches(&v) && cb.matches(&v) {
                    return true;
                }
            }
        }
    }
    false
}

/// Tiebreak hierarchy: success_rate > weight > alphabetical agent name.
fn pick_a_over_b(fit: &FitnessCtx) -> bool {
    if (fit.a_success_rate - fit.b_success_rate).abs() > 1e-9 {
        return fit.a_success_rate > fit.b_success_rate;
    }
    if (fit.a_weight - fit.b_weight).abs() > 1e-9 {
        return fit.a_weight > fit.b_weight;
    }
    fit.a_agent < fit.b_agent
}

#[cfg(test)]
mod union_tests {
    use super::*;
    use mur_common::skill::gene::{McpGene, StepGene, TriggerGene};
    use mur_common::skill::mcp::SkillCapability;
    use mur_common::skill::types::TriggerKind;

    fn empty_gene() -> SkillGene {
        SkillGene {
            triggers: BTreeSet::new(),
            steps: vec![],
            requires: BTreeMap::new(),
            mcp: BTreeSet::new(),
        }
    }

    fn trigger(k: TriggerKind, p: &str) -> TriggerGene {
        TriggerGene {
            kind: k,
            pattern: Some(p.to_string()),
        }
    }

    fn step(intent: &str, desc: &str) -> StepGene {
        StepGene {
            intent: Some(intent.into()),
            description: desc.into(),
            tool: None,
        }
    }

    #[test]
    fn union_combines_triggers() {
        let mut a = empty_gene();
        a.triggers.insert(trigger(TriggerKind::Command, "/a"));
        let mut b = empty_gene();
        b.triggers.insert(trigger(TriggerKind::Command, "/b"));
        let out = union(&a, &b).unwrap();
        assert_eq!(out.triggers.len(), 2);
    }

    #[test]
    fn union_interleaves_steps_round_robin() {
        let mut a = empty_gene();
        a.steps = vec![step("a1", "A1"), step("a2", "A2")];
        let mut b = empty_gene();
        b.steps = vec![step("b1", "B1"), step("b2", "B2"), step("b3", "B3")];
        let out = union(&a, &b).unwrap();
        let descs: Vec<&str> = out.steps.iter().map(|s| s.description.as_str()).collect();
        assert_eq!(descs, vec!["A1", "B1", "A2", "B2", "B3"]);
    }

    #[test]
    fn union_merges_mcp_set() {
        let mut a = empty_gene();
        a.mcp.insert(McpGene {
            tool_pattern: "browser.*".into(),
            capability: SkillCapability::ReadFile,
        });
        let mut b = empty_gene();
        b.mcp.insert(McpGene {
            tool_pattern: "fs.read.*".into(),
            capability: SkillCapability::ReadFile,
        });
        let out = union(&a, &b).unwrap();
        assert_eq!(out.mcp.len(), 2);
    }

    #[test]
    fn union_merges_compatible_semver_strictly() {
        let mut a = empty_gene();
        a.requires.insert("dep".into(), ">=1.0.0".into());
        let mut b = empty_gene();
        b.requires.insert("dep".into(), "<2.0.0".into());
        let out = union(&a, &b).unwrap();
        let merged = out.requires.get("dep").unwrap();
        assert!(merged.contains(">=1.0.0") && merged.contains("<2.0.0"));
    }

    #[test]
    fn union_errors_on_disjoint_semver() {
        let mut a = empty_gene();
        a.requires.insert("dep".into(), ">=2.0.0".into());
        let mut b = empty_gene();
        b.requires.insert("dep".into(), "<1.0.0".into());
        assert!(matches!(
            union(&a, &b),
            Err(StrategyError::DisjointSemver { .. })
        ));
    }

    #[test]
    fn union_preserves_unique_requires_from_each_side() {
        let mut a = empty_gene();
        a.requires.insert("a-only".into(), "1.0.0".into());
        let mut b = empty_gene();
        b.requires.insert("b-only".into(), "2.0.0".into());
        let out = union(&a, &b).unwrap();
        assert_eq!(out.requires.len(), 2);
    }
}

#[cfg(test)]
mod intersection_tests {
    use super::*;
    use mur_common::skill::gene::{StepGene, TriggerGene};
    use mur_common::skill::types::TriggerKind;

    fn ctx(a_rate: f64, b_rate: f64) -> FitnessCtx {
        FitnessCtx {
            a_agent: "alice".into(),
            b_agent: "bob".into(),
            a_success_rate: a_rate,
            b_success_rate: b_rate,
            a_weight: 0.5,
            b_weight: 0.5,
        }
    }

    fn gene_with(
        triggers: Vec<TriggerGene>,
        steps: Vec<StepGene>,
        requires: Vec<(&str, &str)>,
    ) -> SkillGene {
        SkillGene {
            triggers: triggers.into_iter().collect(),
            steps,
            requires: requires
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            mcp: BTreeSet::new(),
        }
    }

    fn t(p: &str) -> TriggerGene {
        TriggerGene {
            kind: TriggerKind::Command,
            pattern: Some(p.into()),
        }
    }

    fn s(intent: &str, desc: &str) -> StepGene {
        StepGene {
            intent: Some(intent.into()),
            description: desc.into(),
            tool: None,
        }
    }

    #[test]
    fn intersection_keeps_only_shared_triggers() {
        let a = gene_with(vec![t("/x"), t("/y")], vec![s("i1", "A")], vec![]);
        let b = gene_with(vec![t("/y"), t("/z")], vec![s("i1", "B")], vec![]);
        let out = intersection(&a, &b, &ctx(0.5, 0.5)).unwrap();
        assert_eq!(out.triggers.len(), 1);
        assert_eq!(
            out.triggers.iter().next().unwrap().pattern.as_deref(),
            Some("/y")
        );
    }

    #[test]
    fn intersection_picks_higher_success_step() {
        let a = gene_with(vec![t("/x")], vec![s("i1", "from-a")], vec![]);
        let b = gene_with(vec![t("/x")], vec![s("i1", "from-b")], vec![]);
        let out = intersection(&a, &b, &ctx(0.9, 0.5)).unwrap();
        assert_eq!(out.steps[0].description, "from-a");
        let out2 = intersection(&a, &b, &ctx(0.5, 0.9)).unwrap();
        assert_eq!(out2.steps[0].description, "from-b");
    }

    #[test]
    fn intersection_tiebreaks_by_weight_then_alphabetical() {
        let a = gene_with(vec![t("/x")], vec![s("i1", "from-a")], vec![]);
        let b = gene_with(vec![t("/x")], vec![s("i1", "from-b")], vec![]);
        let mut c = ctx(0.5, 0.5);
        c.a_weight = 0.7;
        c.b_weight = 0.3;
        assert_eq!(
            intersection(&a, &b, &c).unwrap().steps[0].description,
            "from-a"
        );
        let c2 = ctx(0.5, 0.5);
        assert_eq!(
            intersection(&a, &b, &c2).unwrap().steps[0].description,
            "from-a"
        );
    }

    #[test]
    fn intersection_drops_unmatched_intent_steps() {
        let a = gene_with(
            vec![t("/x")],
            vec![s("i1", "A1"), s("i2", "A2")],
            vec![],
        );
        let b = gene_with(vec![t("/x")], vec![s("i1", "B1")], vec![]);
        let out = intersection(&a, &b, &ctx(0.5, 0.5)).unwrap();
        assert_eq!(out.steps.len(), 1);
    }

    #[test]
    fn intersection_errors_on_empty_trigger_overlap() {
        let a = gene_with(vec![t("/x")], vec![s("i", "A")], vec![]);
        let b = gene_with(vec![t("/y")], vec![s("i", "B")], vec![]);
        assert!(matches!(
            intersection(&a, &b, &ctx(0.5, 0.5)),
            Err(StrategyError::EmptyIntersection { what: "triggers" })
        ));
    }

    #[test]
    fn intersection_errors_on_empty_step_overlap() {
        let a = gene_with(vec![t("/x")], vec![s("i1", "A")], vec![]);
        let b = gene_with(vec![t("/x")], vec![s("i2", "B")], vec![]);
        assert!(matches!(
            intersection(&a, &b, &ctx(0.5, 0.5)),
            Err(StrategyError::EmptyIntersection { what: "steps" })
        ));
    }
}
