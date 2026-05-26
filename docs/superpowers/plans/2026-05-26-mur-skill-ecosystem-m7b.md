# M7b — Skill Gene Model + Recombination Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `mur skill recombine` — produce a new skill on the invoking agent by combining two parent skills (local or peer) under Union, Intersection, or LLM strategy. Field-level gene model. Output strictly on invoking agent (preserves M7a's no-peer-write invariant).

**Architecture:** A pure `SkillGene` data layer in `mur-common`, three composable strategies in `mur-core/cross_agent/recombine/` (Union and Intersection sync; LLM async via M6c `maintenance_call`), a thin orchestrator that loads refs (local or `agent://peer/skill`), runs the chosen strategy, validates via M6a, and persists as a `Draft` skill on the invoking agent.

**Tech Stack:** Rust 2024. No new crates. Reuses `mur_common::skill::constraint::Constraint` (semver), `mur_common::skill::validate::validate` (M6a), `mur_core::skill_llm::maintenance_call` (M6c, async), `mur_common::skill::peers::list_peer_agents` (M7a), `chrono`, `serde_yaml_ng`, `serde_json`, `tokio` (already a workspace dep via dispatcher).

**Spec:** `docs/superpowers/specs/2026-05-26-mur-skill-ecosystem-m7b-design.md`.

**Scoping doc:** `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7-scoping.md` §3 M7b.

**Hard dependencies (already in repo):**
- M5a — `SkillStats::path_agent`, `SkillStats::load/save`, lifecycle states (Draft).
- M6a — `mur_common::skill::validate::validate(&SkillManifest) -> Result<(), ValidationError>`.
- M6c — `mur_core::skill_llm::{maintenance_call, resolve_maintenance_model, MaintenanceCtx, TokenBudget, SkillLlmError}`. Existing async pattern in `mur-core/src/cmd/skill_doctor.rs:607` and `mur-core/src/cmd/skill_consolidate.rs:12`.
- M7a — `mur_common::skill::peers::{list_peer_agents, PeerAgent}`, `mur_core::cross_agent::fitness::{fitness, AgentFitness}`.
- Existing: `mur_common::skill::parser::parse_canonical`, `mur_common::skill::manifest::SkillManifest`, `mur_common::skill::local::list_installed_agent`, `mur_common::skill::constraint::Constraint`.

**What M7b ships:**
1. `mur-common/src/skill/gene.rs` — `SkillGene`, `StepGene`, `TriggerGene`, `McpGene`, `GeneDiff`. Pure derivation from `SkillManifest`. Procedure-mode skills only.
2. `EvolutionEvent::recombined(...)` constructor.
3. `mur-core/src/cross_agent/recombine/` module: `mod.rs` (orchestrator), `strategy.rs` (Union + Intersection), `llm.rs` (LLM strategy), `peer_ref.rs` (`SkillRef` parse + load).
4. `mur skill recombine` CLI subcommand + dispatcher.
5. Integration test suite covering same-agent, cross-agent, dry-run, evolution log, name collision, LLM error path.

**What M7b does NOT ship:**
- Automatic propagation / idle hook → M7c.
- Credit ledger → M7c.
- Intent canonicaliser → M7c.
- Writing to peer state under any condition (invariant from M7a).
- Vector / embedding-based gene diff → M7+.
- N-way recombine (≥3 parents) → out of scope.
- Recombine of non-procedure-mode skills (context-only or command-only) → errors with a clear message; out of scope.

---

## File Structure

**Create:**
- `mur-common/src/skill/gene.rs` — `SkillGene`, `StepGene`, `TriggerGene`, `McpGene`, `GeneDiff`, `from_manifest`, `diff`. ~250 lines.
- `mur-core/src/cross_agent/recombine/mod.rs` — `RecombineOptions`, `RecombineOutcome`, `run_recombine` orchestrator. ~200 lines.
- `mur-core/src/cross_agent/recombine/strategy.rs` — `RecombineStrategy`, `union`, `intersection`, `FitnessCtx`, semver merge helper. ~280 lines.
- `mur-core/src/cross_agent/recombine/llm.rs` — `llm_recombine` async dispatcher, prompt template. ~150 lines.
- `mur-core/src/cross_agent/recombine/peer_ref.rs` — `SkillRef`, `parse_ref`, `LoadedSkillRef`, `load_skill_ref`. ~120 lines.
- `mur-core/src/cmd/skill_recombine.rs` — `cmd_recombine` async CLI dispatcher + output formatters. ~180 lines.
- `mur-core/tests/skill_recombine.rs` — integration suite (6 tests). ~250 lines.

**Modify:**
- `mur-common/src/skill/mod.rs` — `pub mod gene;` + `pub use gene::{SkillGene, GeneDiff};`.
- `mur-common/src/skill/evolution.rs` — add `EvolutionEvent::recombined(...)` constructor.
- `mur-core/src/cross_agent/mod.rs` — add `pub mod recombine;`.
- `mur-core/src/cli/skill.rs` — add `Recombine` variant.
- `mur-core/src/dispatch.rs` — wire `SkillAction::Recombine` to `cmd::skill_recombine::cmd_recombine`.

**Do not modify:**
- `mur_common::skill::manifest` — read-only.
- M5b's `run_consolidate` / `SkillView` — not touched.
- M7a's `consolidate.rs` / `fitness.rs` / `stats_agg.rs` — not touched (we import from them).
- `SkillStats` schema — additive-only contract from M5b; M7b needs no new fields.

---

### Task 1 — `SkillGene` data layer (pure, no I/O)

**Files:** `mur-common/src/skill/gene.rs` (new), `mur-common/src/skill/mod.rs` (modify).

- [ ] **Step 1: Add module export**

In `mur-common/src/skill/mod.rs`, insert (keep alphabetical with sibling `pub mod` lines around line 7):

```rust
pub mod gene;
```

And under the re-export block (near line 29):

```rust
pub use gene::{GeneDiff, McpGene, SkillGene, StepGene, TriggerGene};
```

- [ ] **Step 2: Create `gene.rs` with the data types**

Create `mur-common/src/skill/gene.rs`:

```rust
//! Skill gene model (M7b).
//!
//! A `SkillGene` is a pure field-level projection of a `SkillManifest`. It is
//! not persisted — derived on demand. Two genes can be diffed and recombined
//! to produce a third manifest.
//!
//! Scope (M7b): procedure-mode skills only. Context-only or command-only
//! skills are not eligible — `from_manifest` returns `Err` for them.

use crate::skill::manifest::{
    McpRequirement, Procedure, ProcedureStep, Requirement, SkillManifest, Trigger,
};
use crate::skill::mcp::SkillCapability;
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
        let proc = m.content.procedure.as_ref().ok_or(GeneError::NotProcedure)?;

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

        Ok(SkillGene { triggers, steps, requires, mcp })
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
                })
                .collect(),
        }
    }

    pub fn to_triggers(&self) -> Vec<Trigger> {
        self.triggers
            .iter()
            .map(|t| Trigger { kind: t.kind, pattern: t.pattern.clone() })
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
        let mut d = GeneDiff::default();

        // Triggers — set diff
        d.triggers_added = b.triggers.difference(&a.triggers).cloned().collect();
        d.triggers_removed = a.triggers.difference(&b.triggers).cloned().collect();

        // MCP — set diff
        d.mcp_added = b.mcp.difference(&a.mcp).cloned().collect();
        d.mcp_removed = a.mcp.difference(&b.mcp).cloned().collect();

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
    use crate::skill::manifest::{Content, Procedure, ProcedureStep, Trigger};
    use crate::skill::types::{Category, TriggerKind};

    fn manifest_with_steps(steps: Vec<ProcedureStep>, triggers: Vec<Trigger>) -> SkillManifest {
        SkillManifest {
            name: "x".into(),
            version: "0.1.0".into(),
            publisher: "human:test".into(),
            description: "t".into(),
            category: Category::Workflow,
            hosts: vec![],
            content: Content {
                r#abstract: "a".into(),
                context: None,
                procedure: Some(Procedure { variables: vec![], steps }),
                command: None,
            },
            requires: vec![],
            tags: vec![],
            triggers,
            priority: Default::default(),
            evolution_log: vec![],
            transfer_chain: vec![],
            mcp_requirements: vec![],
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
            }],
            vec![Trigger { kind: TriggerKind::Command, pattern: Some("/x".into()) }],
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
        assert!(matches!(SkillGene::from_manifest(&m), Err(GeneError::NotProcedure)));
    }

    #[test]
    fn diff_detects_added_and_changed_steps() {
        let a = manifest_with_steps(
            vec![ProcedureStep {
                description: "old".into(),
                tool: None,
                intent: Some("i1".into()),
                tool_hint: None,
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
                },
                ProcedureStep {
                    description: "added".into(),
                    tool: None,
                    intent: Some("i2".into()),
                    tool_hint: None,
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
```

- [ ] **Step 3: Verify build and tests**

```bash
cargo build -p mur-common
cargo test -p mur-common skill::gene
```

Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/skill/gene.rs mur-common/src/skill/mod.rs
git commit -m "feat(skill): M7b gene model — SkillGene, GeneDiff, field-level projection of SkillManifest"
```

---

### Task 2 — `EvolutionEvent::recombined` constructor

**Files:** `mur-common/src/skill/evolution.rs` (modify).

`EvolutionEvent` is a struct (not enum). We add a constructor that fills `source = "agent:recombiner"` and packs parents/strategy/output into `changes` using a deterministic format. The format is `recombine: a=<ref_a>, b=<ref_b>, strategy=<s>, output=<name>` — easy to grep and parse later in M7c lineage queries.

- [ ] **Step 1: Add a failing test**

Append to `mur-common/src/skill/evolution.rs` test module:

```rust
    #[test]
    fn recombined_sets_recombiner_source_and_packs_metadata() {
        let event = EvolutionEvent::recombined(
            "0.1.0",
            5,
            "local/research-prices",
            "agent://bob/lookup",
            "union",
            "combined-research",
        );
        assert_eq!(event.source, "agent:recombiner");
        assert_eq!(event.version, "0.1.0");
        assert_eq!(event.generation, 5);
        assert!(event.changes.starts_with("recombine: "));
        assert!(event.changes.contains("a=local/research-prices"));
        assert!(event.changes.contains("b=agent://bob/lookup"));
        assert!(event.changes.contains("strategy=union"));
        assert!(event.changes.contains("output=combined-research"));
        assert!(event.quality_score.is_none());
        assert!(!event.timestamp.is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p mur-common evolution::tests::recombined_sets_recombiner_source_and_packs_metadata
```

Expected: FAIL — "no associated function `recombined`".

- [ ] **Step 3: Implement the constructor**

Insert below the existing `evolved` constructor in `mur-common/src/skill/evolution.rs`:

```rust
    /// Constructor for M7b recombination events. `parent_a` and `parent_b`
    /// are stringified `SkillRef`s (e.g. `local/foo` or `agent://bob/bar`).
    pub fn recombined(
        version: &str,
        generation: u32,
        parent_a: &str,
        parent_b: &str,
        strategy: &str,
        output_skill: &str,
    ) -> Self {
        Self {
            version: version.to_string(),
            generation,
            source: "agent:recombiner".into(),
            changes: format!(
                "recombine: a={parent_a}, b={parent_b}, strategy={strategy}, output={output_skill}"
            ),
            quality_score: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p mur-common evolution::tests
```

Expected: 4 tests pass (existing 3 + new 1).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/evolution.rs
git commit -m "feat(skill): M7b EvolutionEvent::recombined — recombiner provenance constructor"
```

---

### Task 3 — Module scaffold + `RecombineStrategy` enum

**Files:** `mur-core/src/cross_agent/mod.rs` (modify), `mur-core/src/cross_agent/recombine/mod.rs` (new), `mur-core/src/cross_agent/recombine/strategy.rs` (new — skeleton only).

This is a quick scaffold so subsequent tasks compile.

- [ ] **Step 1: Wire module in `cross_agent/mod.rs`**

Append to `mur-core/src/cross_agent/mod.rs`:

```rust
pub mod recombine;
```

- [ ] **Step 2: Create skeleton `recombine/mod.rs`**

`mur-core/src/cross_agent/recombine/mod.rs`:

```rust
//! M7b — Skill recombination engine.
//!
//! Two parent skills produce a third under one of three strategies:
//! Union (superset merge), Intersection (overlap merge), LLM (delegated).
//! Output strictly on the invoking agent — peer state is never written.

pub mod llm;
pub mod peer_ref;
pub mod strategy;

pub use strategy::{FitnessCtx, RecombineStrategy};
```

- [ ] **Step 3: Create `strategy.rs` with just the enum**

`mur-core/src/cross_agent/recombine/strategy.rs`:

```rust
//! Recombination strategies.

use serde::{Deserialize, Serialize};

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
```

- [ ] **Step 4: Create placeholder `llm.rs` and `peer_ref.rs`**

`mur-core/src/cross_agent/recombine/llm.rs`:

```rust
//! LLM strategy — fills in Task 6.
```

`mur-core/src/cross_agent/recombine/peer_ref.rs`:

```rust
//! Peer reference parser + loader — fills in Task 5.
```

- [ ] **Step 5: Verify build**

```bash
cargo build -p mur-core
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cross_agent/mod.rs mur-core/src/cross_agent/recombine/
git commit -m "feat(skill): M7b recombine module scaffold + strategy enum"
```

---

### Task 4 — Union strategy

**Files:** `mur-core/src/cross_agent/recombine/strategy.rs` (modify).

Union semantics: triggers ∪, mcp ∪, requires merged via semver intersection (stricter wins, disjoint = error), steps interleaved A0,B0,A1,B1,…

- [ ] **Step 1: Add failing tests at the bottom of `strategy.rs`**

```rust
#[cfg(test)]
mod union_tests {
    use super::*;
    use mur_common::skill::gene::{McpGene, SkillGene, StepGene, TriggerGene};
    use mur_common::skill::mcp::SkillCapability;
    use mur_common::skill::types::TriggerKind;
    use std::collections::{BTreeMap, BTreeSet};

    fn empty_gene() -> SkillGene {
        SkillGene {
            triggers: BTreeSet::new(),
            steps: vec![],
            requires: BTreeMap::new(),
            mcp: BTreeSet::new(),
        }
    }

    fn trigger(k: TriggerKind, p: &str) -> TriggerGene {
        TriggerGene { kind: k, pattern: Some(p.to_string()) }
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
            capability: SkillCapability::ReadOnly,
        });
        let mut b = empty_gene();
        b.mcp.insert(McpGene {
            tool_pattern: "fs.read.*".into(),
            capability: SkillCapability::ReadOnly,
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
        // Stricter semver is intersection — both must hold.
        let merged = out.requires.get("dep").unwrap();
        assert!(merged.contains(">=1.0.0") && merged.contains("<2.0.0"));
    }

    #[test]
    fn union_errors_on_disjoint_semver() {
        let mut a = empty_gene();
        a.requires.insert("dep".into(), ">=2.0.0".into());
        let mut b = empty_gene();
        b.requires.insert("dep".into(), "<1.0.0".into());
        assert!(matches!(union(&a, &b), Err(StrategyError::DisjointSemver { .. })));
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
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p mur-core cross_agent::recombine::strategy::union_tests 2>&1 | head -30
```

Expected: FAIL — `union` and `StrategyError` not defined.

- [ ] **Step 3: Implement Union + semver merge helper + error type**

Replace the `strategy.rs` file with the following (keep the existing `RecombineStrategy` and `FitnessCtx` at the top; this adds Union + helper + error):

```rust
//! Recombination strategies.

use mur_common::skill::constraint::{Constraint, ConstraintError};
use mur_common::skill::gene::SkillGene;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

    Ok(SkillGene { triggers, steps, requires, mcp })
}

/// Combine two semver constraint strings into the strictest constraint that
/// satisfies both. Returns `Err(DisjointSemver)` when no version satisfies
/// both inputs.
pub fn merge_semver(name: &str, a: &str, b: &str) -> Result<String, StrategyError> {
    // Parse both via the existing skill Constraint type (wraps semver::VersionReq).
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

    // The combined constraint is the conjunction. semver::VersionReq doesn't
    // support intersection natively, so we represent it as the comma-joined
    // string (which VersionReq parses as AND). Then we sanity-check that at
    // least one well-known version satisfies both — a lightweight non-empty
    // check using a probe over major versions 0..100.
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
/// satisfiable. Not exhaustive — semver requires would need a SAT solver for
/// that — but catches the common "disjoint upper/lower" failure mode.
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
```

Add `semver` to `mur-core/Cargo.toml` if not present (it is — confirm with `grep semver mur-core/Cargo.toml`; if absent, add `semver = { workspace = true }`).

- [ ] **Step 4: Verify Cargo.toml has `semver`**

```bash
grep -E "^semver" mur-core/Cargo.toml || echo "semver = { workspace = true }" >> mur-core/Cargo.toml
```

If the file uses a `[dependencies]` block, append `semver = { workspace = true }` inside that block (manual edit) rather than to the file tail.

- [ ] **Step 5: Run tests to verify pass**

```bash
cargo test -p mur-core cross_agent::recombine::strategy::union_tests
```

Expected: 6 tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cross_agent/recombine/strategy.rs mur-core/Cargo.toml
git commit -m "feat(skill): M7b union strategy + strict semver merge"
```

---

### Task 5 — Intersection strategy

**Files:** `mur-core/src/cross_agent/recombine/strategy.rs` (modify).

Intersection semantics: triggers ∩, mcp ∩, requires ∩ via semver merge (same merger as Union — both must hold), steps matched by `intent` (Some-only); keeper picked via fitness tiebreak. Empty result → error.

- [ ] **Step 1: Add failing tests**

Append to `strategy.rs` (above existing tests is fine, or as a new test module):

```rust
#[cfg(test)]
mod intersection_tests {
    use super::*;
    use mur_common::skill::gene::{SkillGene, StepGene, TriggerGene};
    use mur_common::skill::types::TriggerKind;
    use std::collections::{BTreeMap, BTreeSet};

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
        TriggerGene { kind: TriggerKind::Command, pattern: Some(p.into()) }
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
        assert_eq!(out.triggers.iter().next().unwrap().pattern.as_deref(), Some("/y"));
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
        // Equal rates → weight tiebreak
        let mut c = ctx(0.5, 0.5);
        c.a_weight = 0.7;
        c.b_weight = 0.3;
        assert_eq!(intersection(&a, &b, &c).unwrap().steps[0].description, "from-a");
        // Equal rates and weights → alphabetical (alice < bob → a wins)
        let c2 = ctx(0.5, 0.5);
        assert_eq!(intersection(&a, &b, &c2).unwrap().steps[0].description, "from-a");
    }

    #[test]
    fn intersection_drops_unmatched_intent_steps() {
        let a = gene_with(vec![t("/x")], vec![s("i1", "A1"), s("i2", "A2")], vec![]);
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
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p mur-core cross_agent::recombine::strategy::intersection_tests 2>&1 | head -20
```

Expected: FAIL — `intersection` not defined.

- [ ] **Step 3: Implement Intersection**

Append to `strategy.rs`:

```rust
/// Intersection strategy — only what both parents share.
pub fn intersection(
    a: &SkillGene,
    b: &SkillGene,
    fit: &FitnessCtx,
) -> Result<SkillGene, StrategyError> {
    use std::collections::{BTreeMap, BTreeSet};

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
    shared_intents.sort();  // deterministic order

    if shared_intents.is_empty() {
        return Err(StrategyError::EmptyIntersection { what: "steps" });
    }

    let prefer_a = pick_a_over_b(fit);
    let mut steps = Vec::with_capacity(shared_intents.len());
    for intent in shared_intents {
        let pick = if prefer_a { a_by_intent[intent] } else { b_by_intent[intent] };
        steps.push(pick.clone());
    }

    Ok(SkillGene { triggers, steps, requires, mcp })
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
```

- [ ] **Step 4: Run tests to verify pass**

```bash
cargo test -p mur-core cross_agent::recombine::strategy
```

Expected: 12 tests pass (6 union + 6 intersection).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cross_agent/recombine/strategy.rs
git commit -m "feat(skill): M7b intersection strategy with fitness-based step tiebreak"
```

---

### Task 6 — Peer ref parser + loader

**Files:** `mur-core/src/cross_agent/recombine/peer_ref.rs` (modify — replace placeholder).

- [ ] **Step 1: Add failing tests at the bottom of `peer_ref.rs`**

Replace `peer_ref.rs` placeholder with this skeleton + tests:

```rust
//! Parse and load skill references.
//!
//! A ref is either `<name>` (local on invoking agent) or
//! `agent://<peer>/<name>` (read-only from peer).

use anyhow::{Result, anyhow, bail};
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::parser::parse_canonical;
use mur_common::skill::peers::list_peer_agents;
use mur_common::skill::stats::SkillStats;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRef {
    /// `None` = local current agent; `Some(name)` = peer agent.
    pub agent: Option<String>,
    pub skill: String,
}

impl SkillRef {
    pub fn display(&self) -> String {
        match &self.agent {
            Some(a) => format!("agent://{a}/{}", self.skill),
            None => format!("local/{}", self.skill),
        }
    }
}

pub fn parse_ref(s: &str) -> Result<SkillRef> {
    if let Some(rest) = s.strip_prefix("agent://") {
        let mut parts = rest.splitn(2, '/');
        let agent = parts.next().filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("missing peer agent in ref '{s}'"))?;
        let skill = parts.next().filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("missing skill name in ref '{s}'"))?;
        Ok(SkillRef { agent: Some(agent.to_string()), skill: skill.to_string() })
    } else if s.contains('/') {
        bail!("invalid skill ref '{s}': use '<name>' or 'agent://<peer>/<name>'");
    } else {
        Ok(SkillRef { agent: None, skill: s.to_string() })
    }
}

pub struct LoadedSkillRef {
    pub manifest: SkillManifest,
    pub stats: SkillStats,
    /// `"local"` or the peer agent name — for display + EvolutionEvent.
    pub agent_label: String,
    pub ref_: SkillRef,
}

/// Load manifest + stats for a `SkillRef`. `current_agent` is the invoking
/// agent name and is used to resolve `agent: None` (local) refs to the
/// invoker's per-agent skills directory.
pub fn load_skill_ref(
    home: &Path,
    current_agent: &str,
    r: &SkillRef,
) -> Result<LoadedSkillRef> {
    let agent_name = r.agent.as_deref().unwrap_or(current_agent);
    let agent_label = r.agent.clone().unwrap_or_else(|| "local".to_string());

    let agent_root = home.join("agents").join(agent_name);
    if !agent_root.exists() {
        bail!("agent '{agent_name}' not found at {}", agent_root.display());
    }

    let manifest_path = agent_root.join("skills").join(&r.skill).join("skill.yaml");
    if !manifest_path.exists() {
        let installed = installed_skills(&agent_root);
        bail!(
            "skill '{}' not found on agent '{agent_name}'. Installed: {}",
            r.skill,
            installed.join(", ")
        );
    }

    let yaml = std::fs::read_to_string(&manifest_path)?;
    let manifest = parse_canonical(&yaml).map_err(|e| anyhow!("parse {manifest_path:?}: {e}"))?;

    let stats_path = SkillStats::path_agent(home, agent_name, &r.skill);
    let stats = SkillStats::load(&stats_path)?
        .unwrap_or_else(|| SkillStats::new(&r.skill, "unknown", "", chrono::Utc::now()));

    Ok(LoadedSkillRef {
        manifest,
        stats,
        agent_label,
        ref_: r.clone(),
    })
}

fn installed_skills(agent_root: &Path) -> Vec<String> {
    let dir = agent_root.join("skills");
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

// Suppress unused warning until callers (Task 7) wire it in.
#[allow(dead_code)]
fn _force_used() {
    let _ = list_peer_agents;
    let _ = PathBuf::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_local_ref() {
        let r = parse_ref("research-prices").unwrap();
        assert_eq!(r.agent, None);
        assert_eq!(r.skill, "research-prices");
        assert_eq!(r.display(), "local/research-prices");
    }

    #[test]
    fn parse_peer_ref() {
        let r = parse_ref("agent://bob/lookup").unwrap();
        assert_eq!(r.agent.as_deref(), Some("bob"));
        assert_eq!(r.skill, "lookup");
        assert_eq!(r.display(), "agent://bob/lookup");
    }

    #[test]
    fn parse_rejects_bare_slash() {
        assert!(parse_ref("foo/bar").is_err());
    }

    #[test]
    fn parse_rejects_empty_agent_or_skill() {
        assert!(parse_ref("agent:///bar").is_err());
        assert!(parse_ref("agent://bob/").is_err());
    }

    #[test]
    fn load_errors_when_agent_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = SkillRef { agent: Some("ghost".into()), skill: "x".into() };
        let err = load_skill_ref(tmp.path(), "self", &r).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
```

- [ ] **Step 2: Run tests to verify pass (most are unit tests on pure functions)**

```bash
cargo test -p mur-core cross_agent::recombine::peer_ref
```

Expected: 5 tests pass. (No "failing test" loop here — these are all pure helpers, fastest to TDD as a batch.)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cross_agent/recombine/peer_ref.rs
git commit -m "feat(skill): M7b SkillRef parser + per-agent manifest+stats loader"
```

---

### Task 7 — LLM strategy

**Files:** `mur-core/src/cross_agent/recombine/llm.rs` (modify — replace placeholder).

LLM strategy is the only async part of the strategy layer. It calls `maintenance_call` from M6c. On `Ok(None)` (soft-fail) or model-missing, returns a typed error so the orchestrator can exit with code 4 or 5.

- [ ] **Step 1: Implement `llm_recombine`**

Replace `llm.rs` with:

```rust
//! LLM recombination strategy (M7b).
//!
//! Delegates the merge to `skill_llm::maintenance_call` with a fixed prompt.
//! Result is YAML, parsed via the canonical parser and validated via the
//! M6a schema validator before being returned.

use anyhow::{Result, anyhow, bail};
use chrono::Duration;
use mur_common::model::ModelRegistry;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::parser::parse_canonical;
use mur_common::skill::validate::validate;
use std::path::Path;

use crate::skill_llm::{
    MaintenanceCtx, SkillLlmError, TokenBudget, maintenance_call, resolve_maintenance_model,
};

#[derive(Debug, thiserror::Error)]
pub enum LlmRecombineError {
    #[error("no model configured for skill maintenance — run `mur model add` to configure one")]
    NoModel,
    #[error("LLM returned no usable response (network or backend error)")]
    SoftFailed,
    #[error("LLM output failed schema validation: {0}")]
    Invalid(String),
    #[error("LLM call error: {0}")]
    Other(#[from] SkillLlmError),
}

pub async fn llm_recombine(
    home: &Path,
    a: &SkillManifest,
    b: &SkillManifest,
    output_name: &str,
) -> Result<SkillManifest, LlmRecombineError> {
    // Resolve model
    let registry = ModelRegistry::load_or_default(&home.join("models.yaml"))
        .map_err(|e| LlmRecombineError::Other(SkillLlmError::Other(anyhow!("{e}"))))?;
    let model = resolve_maintenance_model(&registry, None).ok_or(LlmRecombineError::NoModel)?;

    // Build prompt
    let prompt = build_prompt(a, b, output_name)
        .map_err(|e| LlmRecombineError::Other(SkillLlmError::Other(e)))?;

    let ctx = MaintenanceCtx {
        budget_ledger: home.join("skill_llm_budget.json"),
        cache_ttl: Duration::days(30),
        daily_cap_usd: 1.00,
    };

    let response = maintenance_call(&prompt, &model, TokenBudget::DEFAULT, &ctx, &registry)
        .await
        .map_err(LlmRecombineError::Other)?;
    let yaml = response.ok_or(LlmRecombineError::SoftFailed)?;

    // Strip code fences if present (LLMs sometimes return ```yaml ... ``` despite instructions)
    let yaml = strip_code_fence(&yaml);

    // Parse + validate
    let manifest =
        parse_canonical(&yaml).map_err(|e| LlmRecombineError::Invalid(format!("parse: {e}")))?;
    validate(&manifest).map_err(|e| LlmRecombineError::Invalid(format!("validate: {e:?}")))?;

    Ok(manifest)
}

fn build_prompt(a: &SkillManifest, b: &SkillManifest, output_name: &str) -> Result<String> {
    let a_yaml = serde_yaml_ng::to_string(a)?;
    let b_yaml = serde_yaml_ng::to_string(b)?;
    Ok(format!(
        r#"You are recombining two YAML skill manifests into a single offspring manifest.

Parent A:
```yaml
{a_yaml}```

Parent B:
```yaml
{b_yaml}```

Rules:
- Output ONLY a single YAML document; no prose, no code fences.
- Set `name: {output_name}` and bump `version` to a fresh `0.1.0`.
- Preserve the same top-level shape as the parents (category, content.procedure, triggers, etc.).
- Combine triggers and requirements pragmatically; avoid duplicates.
- Steps: produce a coherent ordered sequence that achieves both parents' intents.
- Keep `mcp_requirements` minimal — only capabilities used by your output steps.
- Do not invent new tool names; reuse what either parent uses.

Output:
"#
    ))
}

fn strip_code_fence(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```yaml") {
        if let Some(inner) = rest.trim_start_matches('\n').strip_suffix("```") {
            return inner.trim_end().to_string();
        }
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        if let Some(inner) = rest.trim_start_matches('\n').strip_suffix("```") {
            return inner.trim_end().to_string();
        }
    }
    trimmed.to_string()
}

// Suppress unused warnings while orchestrator wiring lands in Task 8.
#[allow(dead_code)]
fn _force_used() {
    let _ = bail::<(), &str>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_yaml_code_fence() {
        let s = "```yaml\nname: x\nversion: 1.0.0\n```";
        assert_eq!(strip_code_fence(s), "name: x\nversion: 1.0.0");
    }

    #[test]
    fn strips_bare_code_fence() {
        let s = "```\nname: x\n```";
        assert_eq!(strip_code_fence(s), "name: x");
    }

    #[test]
    fn passthrough_without_fence() {
        assert_eq!(strip_code_fence("name: x\n"), "name: x");
    }
}
```

Note: if `ModelRegistry::load_or_default` does not exist with that signature, use whatever loader the existing M6c callers use. Inspect `mur-core/src/cmd/skill_doctor.rs:600-620` for the exact pattern; mirror it. (The exact registry-load idiom may differ; align with the doctor's working code rather than inventing one.)

- [ ] **Step 2: Verify build (no LLM live test in this task)**

```bash
cargo build -p mur-core
cargo test -p mur-core cross_agent::recombine::llm::tests
```

Expected: build clean, 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cross_agent/recombine/llm.rs
git commit -m "feat(skill): M7b LLM recombine strategy via skill_llm maintenance_call"
```

---

### Task 8 — Orchestrator `run_recombine`

**Files:** `mur-core/src/cross_agent/recombine/mod.rs` (modify — replace scaffold).

The orchestrator ties everything together. Loads two refs → computes `FitnessCtx` → calls the strategy → derives output name → rebuilds `SkillManifest` from the gene → validates via M6a → on dry-run prints YAML, on apply writes manifest + stats + evolution log to invoking agent's home.

- [ ] **Step 1: Add failing integration-style test stub**

Append to `mod.rs` (this gives us a target API to implement against):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::skill::lifecycle::LifecycleState;
    use mur_common::skill::stats::SkillStats;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn write_skill(home: &std::path::Path, agent: &str, name: &str, yaml: &str) -> PathBuf {
        let dir = home.join("agents").join(agent).join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("skill.yaml");
        std::fs::write(&path, yaml).unwrap();
        path
    }

    fn minimal_yaml(name: &str, trigger: &str, intent: &str, desc: &str) -> String {
        format!(
            r#"name: {name}
version: 0.1.0
publisher: human:test
description: test skill
category: workflow
content:
  abstract: a
  procedure:
    steps:
      - description: {desc}
        intent: {intent}
triggers:
  - type: command
    pattern: "{trigger}"
priority: normal
"#
        )
    }

    #[tokio::test]
    async fn dry_run_does_not_write_output_skill() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_skill(home, "self", "a", &minimal_yaml("a", "/a", "i1", "do A"));
        write_skill(home, "self", "b", &minimal_yaml("b", "/b", "i2", "do B"));

        let opts = RecombineOptions {
            a_ref: peer_ref::parse_ref("a").unwrap(),
            b_ref: peer_ref::parse_ref("b").unwrap(),
            strategy: RecombineStrategy::Union,
            output_name: Some("a-x-b".into()),
            dry_run: true,
            current_agent: "self".into(),
        };
        let outcome = run_recombine(home, &opts).await.unwrap();
        assert!(outcome.written_to.is_none());
        assert!(!outcome.evolution_event_appended);
        let out_path = home.join("agents/self/skills/a-x-b/skill.yaml");
        assert!(!out_path.exists());
    }

    #[tokio::test]
    async fn apply_writes_manifest_stats_and_evolution_log() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_skill(home, "self", "a", &minimal_yaml("a", "/a", "i1", "do A"));
        write_skill(home, "self", "b", &minimal_yaml("b", "/b", "i2", "do B"));

        let opts = RecombineOptions {
            a_ref: peer_ref::parse_ref("a").unwrap(),
            b_ref: peer_ref::parse_ref("b").unwrap(),
            strategy: RecombineStrategy::Union,
            output_name: Some("merged".into()),
            dry_run: false,
            current_agent: "self".into(),
        };
        let outcome = run_recombine(home, &opts).await.unwrap();
        assert!(outcome.written_to.is_some());
        assert!(outcome.evolution_event_appended);
        let out_path = home.join("agents/self/skills/merged/skill.yaml");
        assert!(out_path.exists());
        let stats_path = SkillStats::path_agent(home, "self", "merged");
        let stats = SkillStats::load(&stats_path).unwrap().unwrap();
        assert!(matches!(stats.lifecycle_state, LifecycleState::Draft));
    }

    #[tokio::test]
    async fn name_collision_errors() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_skill(home, "self", "a", &minimal_yaml("a", "/a", "i1", "do A"));
        write_skill(home, "self", "b", &minimal_yaml("b", "/b", "i2", "do B"));
        write_skill(home, "self", "merged", &minimal_yaml("merged", "/m", "im", "exists"));

        let opts = RecombineOptions {
            a_ref: peer_ref::parse_ref("a").unwrap(),
            b_ref: peer_ref::parse_ref("b").unwrap(),
            strategy: RecombineStrategy::Union,
            output_name: Some("merged".into()),
            dry_run: false,
            current_agent: "self".into(),
        };
        let err = run_recombine(home, &opts).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cargo test -p mur-core cross_agent::recombine::tests 2>&1 | head -30
```

Expected: FAIL — `RecombineOptions`, `run_recombine`, etc. not defined.

- [ ] **Step 3: Implement the orchestrator**

Replace the body of `mur-core/src/cross_agent/recombine/mod.rs` (keep the existing `pub mod` lines + reexports at the top; add this below):

```rust
//! M7b — Skill recombination engine.
//!
//! Two parent skills produce a third under one of three strategies:
//! Union (superset merge), Intersection (overlap merge), LLM (delegated).
//! Output strictly on the invoking agent — peer state is never written.

pub mod llm;
pub mod peer_ref;
pub mod strategy;

pub use strategy::{FitnessCtx, RecombineStrategy};

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use mur_common::skill::evolution::EvolutionEvent;
use mur_common::skill::gene::SkillGene;
use mur_common::skill::lifecycle::LifecycleState;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::stats::SkillStats;
use mur_common::skill::validate::validate;
use std::path::{Path, PathBuf};

use peer_ref::{LoadedSkillRef, SkillRef, load_skill_ref};

#[derive(Debug, Clone)]
pub struct RecombineOptions {
    pub a_ref: SkillRef,
    pub b_ref: SkillRef,
    pub strategy: RecombineStrategy,
    pub output_name: Option<String>,
    pub dry_run: bool,
    pub current_agent: String,
}

#[derive(Debug)]
pub struct RecombineOutcome {
    pub manifest: SkillManifest,
    pub manifest_yaml: String,
    pub written_to: Option<PathBuf>,
    pub evolution_event_appended: bool,
    pub output_name: String,
    pub strategy: RecombineStrategy,
}

pub async fn run_recombine(home: &Path, opts: &RecombineOptions) -> Result<RecombineOutcome> {
    let a = load_skill_ref(home, &opts.current_agent, &opts.a_ref)?;
    let b = load_skill_ref(home, &opts.current_agent, &opts.b_ref)?;

    let output_name = opts
        .output_name
        .clone()
        .unwrap_or_else(|| format!("{}-x-{}", a.manifest.name, b.manifest.name));

    // Name collision: refuse before any work.
    let output_path = home
        .join("agents")
        .join(&opts.current_agent)
        .join("skills")
        .join(&output_name);
    if !opts.dry_run && output_path.exists() {
        bail!(
            "skill '{output_name}' already exists on agent '{}'; pass --name to choose another",
            opts.current_agent
        );
    }

    let manifest = match opts.strategy {
        RecombineStrategy::Union => union_or_intersection(&a, &b, true)?,
        RecombineStrategy::Intersection => union_or_intersection(&a, &b, false)?,
        RecombineStrategy::Llm => {
            llm::llm_recombine(home, &a.manifest, &b.manifest, &output_name)
                .await
                .map_err(|e| anyhow!("LLM strategy failed: {e}"))?
        }
    };

    // For Union/Intersection we synthesised a SkillGene-derived manifest; for
    // LLM the model already produced one. In both cases, set authoritative
    // fields the caller chose (name, version reset, evolution metadata).
    let manifest = finalize_manifest(manifest, &output_name, &a, &b, opts.strategy)?;

    // Schema validate
    validate(&manifest).map_err(|e| anyhow!("recombined manifest failed validation: {e:?}"))?;

    let manifest_yaml = serde_yaml_ng::to_string(&manifest)?;

    if opts.dry_run {
        return Ok(RecombineOutcome {
            manifest,
            manifest_yaml,
            written_to: None,
            evolution_event_appended: false,
            output_name,
            strategy: opts.strategy,
        });
    }

    // Atomic write: temp + rename
    std::fs::create_dir_all(&output_path)?;
    let final_path = output_path.join("skill.yaml");
    let tmp_path = output_path.join("skill.yaml.tmp");
    std::fs::write(&tmp_path, &manifest_yaml)?;
    std::fs::rename(&tmp_path, &final_path)?;

    // Stats sidecar at Draft lifecycle
    let mut stats = SkillStats::new(
        &output_name,
        &manifest.publisher,
        &manifest.version,
        Utc::now(),
    );
    stats.lifecycle_state = LifecycleState::Draft;
    let stats_path = SkillStats::path_agent(home, &opts.current_agent, &output_name);
    if let Some(parent) = stats_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    stats.save(&stats_path)?;

    Ok(RecombineOutcome {
        manifest,
        manifest_yaml,
        written_to: Some(final_path),
        evolution_event_appended: true,
        output_name,
        strategy: opts.strategy,
    })
}

fn union_or_intersection(
    a: &LoadedSkillRef,
    b: &LoadedSkillRef,
    is_union: bool,
) -> Result<SkillManifest> {
    let ga = SkillGene::from_manifest(&a.manifest)
        .map_err(|e| anyhow!("parent A ({}): {e}", a.ref_.display()))?;
    let gb = SkillGene::from_manifest(&b.manifest)
        .map_err(|e| anyhow!("parent B ({}): {e}", b.ref_.display()))?;

    let merged_gene = if is_union {
        strategy::union(&ga, &gb).map_err(|e| anyhow!("{e}"))?
    } else {
        let fit = FitnessCtx {
            a_agent: a.agent_label.clone(),
            b_agent: b.agent_label.clone(),
            a_success_rate: success_rate(&a.stats),
            b_success_rate: success_rate(&b.stats),
            // M7a AgentFitness.weight could be plugged here; for M7b we use
            // a neutral 0.5 unless a richer FitnessCtx is provided later.
            a_weight: 0.5,
            b_weight: 0.5,
        };
        strategy::intersection(&ga, &gb, &fit).map_err(|e| anyhow!("{e}"))?
    };

    // Rebuild the manifest from the merged gene, copying static fields from
    // parent A (description, abstract, category are not "genes" in M7b).
    let mut out = a.manifest.clone();
    out.content.procedure = Some(merged_gene.to_procedure());
    out.triggers = merged_gene.to_triggers();
    out.requires = merged_gene.to_requirements();
    out.mcp_requirements = merged_gene.to_mcp_requirements();
    Ok(out)
}

fn finalize_manifest(
    mut m: SkillManifest,
    output_name: &str,
    a: &LoadedSkillRef,
    b: &LoadedSkillRef,
    strategy: RecombineStrategy,
) -> Result<SkillManifest> {
    m.name = output_name.to_string();
    m.version = "0.1.0".to_string();
    m.publisher = format!("agent:recombiner");

    // Generation = max(parent_generation) + 1
    let max_gen = m
        .evolution_log
        .iter()
        .chain(a.manifest.evolution_log.iter())
        .chain(b.manifest.evolution_log.iter())
        .map(|e| e.generation)
        .max()
        .unwrap_or(0);
    let next_gen = max_gen.saturating_add(1);

    // Reset evolution log to a single Recombined event (this is a new skill).
    m.evolution_log = vec![EvolutionEvent::recombined(
        &m.version,
        next_gen,
        &a.ref_.display(),
        &b.ref_.display(),
        strategy.as_str(),
        output_name,
    )];

    // Reset transfer_chain — the offspring originates here.
    m.transfer_chain = vec![];

    Ok(m)
}

fn success_rate(s: &SkillStats) -> f64 {
    let denom = s.success_count + s.failure_count;
    if denom == 0 {
        0.0
    } else {
        s.success_count as f64 / denom as f64
    }
}
```

Note: if `SkillStats::new` takes a different signature in the current crate, mirror M5b's usage in `mur-core/src/skill_consolidate/` callers exactly. Likewise `SkillStats::save` may be named differently — grep for actual usages (`grep -rn "SkillStats::new\|stats.save" mur-core/src --include='*.rs' | head`) and follow the pattern. The plan's signatures above are best-effort against the visible API; adjust to the live one if discrepancies appear.

- [ ] **Step 4: Run tests to verify pass**

```bash
cargo test -p mur-core cross_agent::recombine::tests
```

Expected: 3 tests pass (dry-run, apply, name-collision).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cross_agent/recombine/mod.rs
git commit -m "feat(skill): M7b recombine orchestrator — load refs, dispatch strategy, write Draft offspring"
```

---

### Task 9 — CLI subcommand + dispatcher

**Files:** `mur-core/src/cli/skill.rs` (modify), `mur-core/src/cmd/skill_recombine.rs` (new), `mur-core/src/dispatch.rs` (modify).

- [ ] **Step 1: Add CLI variant**

In `mur-core/src/cli/skill.rs`, inside the `SkillAction` enum (near the existing `Consolidate { ... }` variant, before the closing `}`), add:

```rust
    /// Recombine two skills into a new Draft offspring on this agent.
    Recombine {
        /// First parent ref: `<name>` (local) or `agent://<peer>/<name>`.
        a: String,
        /// Second parent ref: `<name>` (local) or `agent://<peer>/<name>`.
        b: String,
        /// Combination strategy.
        #[arg(long, value_enum, default_value_t = RecombineStrategyArg::Union)]
        strategy: RecombineStrategyArg,
        /// Output skill name. Default: `<a>-x-<b>`.
        #[arg(long)]
        name: Option<String>,
        /// Print recombined manifest YAML to stdout without writing.
        #[arg(long)]
        dry_run: bool,
        /// Invoking agent (default: current agent from runtime context).
        #[arg(long)]
        agent: Option<String>,
        /// Emit JSON outcome record instead of human text.
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum RecombineStrategyArg {
    Union,
    Intersection,
    Llm,
}
```

Move the second `}` (the enum closer) so the `RecombineStrategyArg` lands outside the enum (the snippet above shows the layout: variant inside enum, then closer, then the ValueEnum).

- [ ] **Step 2: Create CLI dispatcher**

`mur-core/src/cmd/skill_recombine.rs`:

```rust
//! CLI dispatcher for `mur skill recombine` (M7b).

use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::cli::RecombineStrategyArg;
use crate::cross_agent::recombine::peer_ref::parse_ref;
use crate::cross_agent::recombine::{
    RecombineOptions, RecombineOutcome, RecombineStrategy, run_recombine,
};

fn map_strategy(s: RecombineStrategyArg) -> RecombineStrategy {
    match s {
        RecombineStrategyArg::Union => RecombineStrategy::Union,
        RecombineStrategyArg::Intersection => RecombineStrategy::Intersection,
        RecombineStrategyArg::Llm => RecombineStrategy::Llm,
    }
}

pub async fn cmd_recombine(
    home: &Path,
    a: &str,
    b: &str,
    strategy: RecombineStrategyArg,
    name: Option<String>,
    dry_run: bool,
    agent: Option<String>,
    json: bool,
) -> Result<()> {
    let current_agent = agent
        .or_else(|| std::env::var("MUR_AGENT").ok())
        .ok_or_else(|| anyhow::anyhow!("--agent <name> required (or set MUR_AGENT)"))?;

    let opts = RecombineOptions {
        a_ref: parse_ref(a)?,
        b_ref: parse_ref(b)?,
        strategy: map_strategy(strategy),
        output_name: name,
        dry_run,
        current_agent,
    };

    let outcome = run_recombine(home, &opts).await?;

    if json {
        print_json(&outcome)?;
    } else {
        print_human(&outcome);
    }
    Ok(())
}

fn print_human(o: &RecombineOutcome) {
    if let Some(path) = &o.written_to {
        println!(
            "✓ Recombined into '{}' (strategy={}, lifecycle=Draft)",
            o.output_name,
            o.strategy.as_str()
        );
        println!("  Manifest: {}", path.display());
        println!("  Stats:    Draft");
        println!("  Evolution log: 1 Recombined event appended");
    } else {
        println!("--- Dry run (strategy={}) ---", o.strategy.as_str());
        println!("{}", o.manifest_yaml);
        println!("--- End (no files written) ---");
    }
}

fn print_json(o: &RecombineOutcome) -> Result<()> {
    let v = serde_json::json!({
        "output_name": o.output_name,
        "strategy": o.strategy.as_str(),
        "written_to": o.written_to.as_ref().map(|p| p.display().to_string()),
        "evolution_event_appended": o.evolution_event_appended,
        "manifest_yaml": o.manifest_yaml,
    });
    serde_json::to_writer_pretty(std::io::stdout(), &v)?;
    println!();
    Ok(())
}
```

- [ ] **Step 3: Wire into dispatcher**

In `mur-core/src/dispatch.rs`, find the `match` block on `SkillAction` (around line 267 per the search earlier). After the `Consolidate { ... }` arm, add:

```rust
            crate::cli::SkillAction::Recombine {
                a, b, strategy, name, dry_run, agent, json,
            } => {
                let code = cmd::skill_recombine::cmd_recombine(
                    &home, &a, &b, strategy, name, dry_run, agent, json,
                )
                .await;
                if code != 0 {
                    std::process::exit(code);
                }
            }
```

And add the module declaration in `mur-core/src/cmd/mod.rs` (or wherever `pub mod skill_consolidate;` lives):

```rust
pub mod skill_recombine;
```

Note: this is the one CLI in the workspace that uses explicit exit-code mapping (per spec §8). Most other `mur skill` subcommands bubble `anyhow` errors which become exit code 1. Recombine's exit codes are part of its scripting contract, so we trade boilerplate for predictability here.

- [ ] **Step 4: Update `cmd_recombine` signature to return exit code**

Change the dispatcher in `mur-core/src/cmd/skill_recombine.rs` (replace the `pub async fn cmd_recombine` body):

```rust
pub async fn cmd_recombine(
    home: &Path,
    a: &str,
    b: &str,
    strategy: RecombineStrategyArg,
    name: Option<String>,
    dry_run: bool,
    agent: Option<String>,
    json: bool,
) -> i32 {
    let current_agent = match agent.or_else(|| std::env::var("MUR_AGENT").ok()) {
        Some(s) => s,
        None => {
            eprintln!("error: --agent <name> required (or set MUR_AGENT)");
            return 2;
        }
    };

    let a_ref = match parse_ref(a) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let b_ref = match parse_ref(b) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let opts = RecombineOptions {
        a_ref,
        b_ref,
        strategy: map_strategy(strategy),
        output_name: name,
        dry_run,
        current_agent,
    };

    match run_recombine(home, &opts).await {
        Ok(outcome) => {
            if json {
                if let Err(e) = print_json(&outcome) {
                    eprintln!("error: {e}");
                    return 1;
                }
            } else {
                print_human(&outcome);
            }
            0
        }
        Err(e) => {
            let msg = e.to_string();
            let code = classify_error(&msg);
            eprintln!("error: {msg}");
            code
        }
    }
}

/// Map error messages to spec §8 exit codes. Pattern-match on substrings the
/// inner layers produce (agent missing, empty intersection, model missing,
/// validation, name collision). Returns 5 for anything unrecognised.
fn classify_error(msg: &str) -> i32 {
    if msg.contains("not found") {
        2
    } else if msg.contains("intersection produced empty") {
        3
    } else if msg.contains("no model") || msg.contains("mur model add") {
        4
    } else if msg.contains("already exists") {
        6
    } else if msg.contains("disjoint semver") || msg.contains("validation") {
        5
    } else {
        5
    }
}
```

The `print_json` helper returns `Result`; keep it as-is. The `print_human` helper stays unchanged.

- [ ] **Step 4: Verify build**

```bash
cargo build -p mur-core
```

Expected: clean.

- [ ] **Step 5: Manual smoke test**

```bash
cargo run -- skill recombine --help
```

Expected: shows the new subcommand with all flags.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/skill.rs mur-core/src/cmd/skill_recombine.rs mur-core/src/cmd/mod.rs mur-core/src/dispatch.rs
git commit -m "feat(skill): M7b mur skill recombine CLI + dispatcher with exit-code mapping"
```

---

### Task 10 — End-to-end integration tests

**File:** `mur-core/tests/skill_recombine.rs` (new).

These exercise the orchestrator from the outside, with synthetic peer fixtures.

- [ ] **Step 1: Create the integration suite**

`mur-core/tests/skill_recombine.rs`:

```rust
//! M7b integration tests — cross-agent recombination scenarios.

use mur_common::skill::lifecycle::LifecycleState;
use mur_common::skill::stats::SkillStats;
use mur_core::cross_agent::recombine::peer_ref::parse_ref;
use mur_core::cross_agent::recombine::{
    RecombineOptions, RecombineStrategy, run_recombine,
};
use std::path::Path;
use tempfile::TempDir;

fn write_skill(home: &Path, agent: &str, name: &str, yaml: &str) {
    let dir = home.join("agents").join(agent).join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
}

fn write_stats(
    home: &Path,
    agent: &str,
    skill: &str,
    success: u64,
    failure: u64,
    last_used: chrono::DateTime<chrono::Utc>,
) {
    let mut s = SkillStats::new(skill, "human:test", "0.1.0", last_used);
    s.success_count = success;
    s.failure_count = failure;
    s.usage_count = success + failure;
    s.last_used_at = Some(last_used);
    let path = SkillStats::path_agent(home, agent, skill);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    s.save(&path).unwrap();
}

fn skill_yaml(name: &str, trigger: &str, intent: &str, desc: &str) -> String {
    format!(
        r#"name: {name}
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: a
  procedure:
    steps:
      - description: {desc}
        intent: {intent}
triggers:
  - type: command
    pattern: "{trigger}"
priority: normal
"#
    )
}

#[tokio::test]
async fn same_agent_union_writes_offspring() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "do A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "do B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("merged".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    assert_eq!(outcome.output_name, "merged");
    let out_yaml = std::fs::read_to_string(outcome.written_to.unwrap()).unwrap();
    assert!(out_yaml.contains("name: merged"));
    assert!(out_yaml.contains("/a"));
    assert!(out_yaml.contains("/b"));
}

#[tokio::test]
async fn cross_agent_intersection_picks_higher_success_step() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let now = chrono::Utc::now();
    write_skill(home, "self", "find", &skill_yaml("find", "/find", "search", "self version"));
    write_skill(home, "peer1", "find", &skill_yaml("find", "/find", "search", "peer version"));
    write_stats(home, "self", "find", 1, 9, now);  // 10% success
    write_stats(home, "peer1", "find", 9, 1, now); // 90% success

    let opts = RecombineOptions {
        a_ref: parse_ref("find").unwrap(),
        b_ref: parse_ref("agent://peer1/find").unwrap(),
        strategy: RecombineStrategy::Intersection,
        output_name: Some("find-merged".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    let out_yaml = std::fs::read_to_string(outcome.written_to.unwrap()).unwrap();
    assert!(out_yaml.contains("peer version")); // higher success_rate wins
}

#[tokio::test]
async fn dry_run_writes_nothing_to_disk() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("x".into()),
        dry_run: true,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    assert!(outcome.written_to.is_none());
    assert!(!home.join("agents/self/skills/x/skill.yaml").exists());
}

#[tokio::test]
async fn offspring_lands_at_draft_lifecycle() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("c".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    run_recombine(home, &opts).await.unwrap();
    let stats = SkillStats::load(&SkillStats::path_agent(home, "self", "c"))
        .unwrap()
        .unwrap();
    assert!(matches!(stats.lifecycle_state, LifecycleState::Draft));
}

#[tokio::test]
async fn evolution_log_contains_recombined_event() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("d".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let outcome = run_recombine(home, &opts).await.unwrap();
    let entry = &outcome.manifest.evolution_log[0];
    assert_eq!(entry.source, "agent:recombiner");
    assert!(entry.changes.contains("strategy=union"));
    assert!(entry.changes.contains("output=d"));
}

#[tokio::test]
async fn name_collision_returns_error() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));
    write_skill(home, "self", "exists", &skill_yaml("exists", "/x", "ix", "X"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Union,
        output_name: Some("exists".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let err = run_recombine(home, &opts).await.unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn llm_strategy_without_model_returns_no_model_error() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    write_skill(home, "self", "a", &skill_yaml("a", "/a", "i1", "A"));
    write_skill(home, "self", "b", &skill_yaml("b", "/b", "i2", "B"));

    let opts = RecombineOptions {
        a_ref: parse_ref("a").unwrap(),
        b_ref: parse_ref("b").unwrap(),
        strategy: RecombineStrategy::Llm,
        output_name: Some("llm-out".into()),
        dry_run: false,
        current_agent: "self".into(),
    };
    let err = run_recombine(home, &opts).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("LLM") || msg.contains("model") || msg.contains("mur model add"),
        "unexpected error: {msg}"
    );
}
```

- [ ] **Step 2: Run the suite**

```bash
cargo test -p mur-core --test skill_recombine
```

Expected: 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/tests/skill_recombine.rs
git commit -m "test(skill): M7b end-to-end recombine integration suite"
```

---

### Task 11 — Workspace lint + format + full test pass

- [ ] **Step 1: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: clean. If unused-imports / dead-code warnings appear in `peer_ref.rs` or `llm.rs` (the `_force_used` helpers), remove them now since real callers (orchestrator + CLI) are wired.

- [ ] **Step 2: Run fmt check**

```bash
cargo fmt --check
```

If failing: `cargo fmt` and commit a `style:` change.

- [ ] **Step 3: Run full workspace test**

```bash
cargo test --workspace
```

Expected: all green, including pre-existing tests.

- [ ] **Step 4: Commit any cleanup**

```bash
git add -u
git commit -m "chore(skill): M7b lint + fmt cleanup"
```

(Skip if nothing to commit.)

---

### Task 12 — Docs

**Files:** `README.md` (modify), `docs/architecture/runtime-overview.md` (modify if a CLI surface section exists).

- [ ] **Step 1: README CLI table row**

Open `README.md`. Find whatever CLI surface table the README has (search for `mur skill consolidate` — the table that lists it). Add one row:

| Command | Description |
|---|---|
| `mur skill recombine <a> <b> [--strategy=…] [--name <out>] [--dry-run]` | M7b: combine two skills (local or `agent://peer/skill`) into a new Draft offspring on the invoking agent |

- [ ] **Step 2: Architecture doc cross-link**

In `docs/architecture/runtime-overview.md`, find the cross-agent section M7a added (search `Cross-agent observability (M7a)`). Add a sibling subsection:

```markdown
### Cross-agent recombination (M7b)

`mur skill recombine <a> <b>` produces a new Draft skill on the invoking agent
by combining two parents (local or `agent://peer/skill`) under Union,
Intersection, or LLM strategy. Output never touches peer state. See
`docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7b.md`.
```

Skip Step 2 if the cross-agent subsection from M7a is not present (don't manufacture one for M7b alone).

- [ ] **Step 3: Commit**

```bash
git add README.md docs/architecture/runtime-overview.md
git commit -m "docs(skill): M7b recombine CLI + architecture cross-reference"
```

---

## Verification Checklist

Before declaring M7b complete:

1. `cargo build --workspace` — clean.
2. `cargo clippy --workspace -- -D warnings` — clean.
3. `cargo fmt --check` — clean.
4. `cargo test --workspace` — all green; includes the 7 integration tests + new unit tests in `gene.rs`, `strategy.rs`, `peer_ref.rs`, `llm.rs`, and `evolution.rs`.
5. Manual smoke on `~/.mur`:
   - `mur skill recombine --help` shows the subcommand
   - With two real local skills on a chosen agent: `mur skill recombine <a> <b> --agent <self> --strategy=union --name test-merge --dry-run` prints a YAML manifest
   - Drop `--dry-run`: `~/.mur/agents/<self>/skills/test-merge/skill.yaml` exists; stats sidecar shows `Draft` lifecycle; evolution_log contains one `agent:recombiner` entry
   - Trust check: `mur skill trust test-merge` (or whichever command reads trust) reports `Sandboxed` — same default as M4b's `agent://` installs. If a different trust default is observed, file a follow-up to set it explicitly in the orchestrator's stats-write step.
   - `mur skill recombine test-merge agent://<peer>/<peer-skill> --strategy=intersection --dry-run` runs (or errors with code 3 if no trigger overlap) — peer's home unchanged in either case
   - Exit codes: `mur skill recombine missing other --agent <self>; echo $?` prints `2`; same shape with `--strategy=llm` and no model configured prints `4`; reusing an existing name prints `6`.
6. Peer state untouched check: list peer directory contents before and after several recombines; diff is empty.

---

## Out of Scope

Carried to M7c:
- `mur agent propagate` automatic propagation
- Idle-trigger hook `skill-propagate`
- Credit ledger `~/.mur/credit/ledger.jsonl` + `mur skill credit`
- Intent canonicaliser + `~/.mur/intent_canonical.yaml`
- Full cross-agent trust inheritance model
- N-way (≥3) recombine

Out of M7 entirely:
- Vector / embedding-based gene diff
- Writing recombined skills back to peer state (invariant)
- LLM strategy with retry / soft-fall-back to Union (predictable errors instead)

---

## Open Questions

All resolved during brainstorming. See `docs/superpowers/specs/2026-05-26-mur-skill-ecosystem-m7b-design.md` §11 for the resolution of scoping-doc Q3/Q4 and which questions still belong to M7c (Q5/Q6).
