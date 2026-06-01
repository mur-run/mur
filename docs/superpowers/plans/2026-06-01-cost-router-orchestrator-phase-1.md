# Cost-Router Orchestrator Phase 1 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Phase 1 of the cost-router orchestrator — a difficulty heuristic + hybrid router layered on the existing `~/.mur/models.yaml` model registry, plus an escalation audit ledger to measure cost savings before spawn is built.

**Architecture:** New `mur_common::route` data types (`RouteTier`, `RouteDecision`, `EscalationEvent`) extend the existing model registry types. A new `mur_core::route` module holds the `Router` (combining heuristic + registry overrides) and `EscalationLedger` (wrapping the generic `Ledger<E>`). CLI surface extends `mur model role` and adds `mur model route {estimate,ledger}`.

**Tech Stack:** Rust (edition 2024), serde YAML, clap derive, the existing `Ledger<E>` from `mur_common::ledger`, the existing `ModelRegistry`/`ModelEntry`/`RoleEntry` from `mur_common::model`.

---

## File Map

| Action | File Path | Responsibility |
|--------|-----------|----------------|
| **Create** | `mur-common/src/route.rs` | `RouteTier`, `RouteDecision`, `EscalationEvent`, `RoutePolicy` types |
| **Modify** | `mur-common/src/model.rs` | Add `tier`, `cost_per_1k_tokens` to `ModelEntry`; add `route_policy` to `RoleEntry` |
| **Modify** | `mur-common/src/lib.rs` | Add `pub mod route` |
| **Create** | `mur-core/src/route/mod.rs` | `Router` struct — combines heuristic + registry + overrides |
| **Create** | `mur-core/src/route/heuristic.rs` | `DifficultyHeuristic` trait + `DefaultHeuristic` impl |
| **Create** | `mur-core/src/route/ledger.rs` | `EscalationLedger` wrapping `Ledger<EscalationEvent>` |
| **Modify** | `mur-core/src/lib.rs` | Add `pub mod route` |
| **Modify** | `mur-core/src/cmd/model.rs` | Add `Route` subcommand (`estimate`, `ledger`); extend `RoleSubCmd::Set` |
| **Modify** | `mur-core/src/cmd/agent/model_resolve.rs`, `cmd/fleet_sync.rs`, `cmd/agent/install.rs`, `cmd/agent/export.rs` | **Compile fix (Task 2):** add `tier`/`cost_per_1k_tokens: None` to existing `ModelEntry` literals — `ModelEntry` has no `Default` derive, so the new fields break every literal |
| **Existing** | `mur-core/src/dispatch.rs` | No change — already dispatches `Commands::Model(args)` → `cmd::model::run(args)` |
| **Existing** | `mur-common/src/ledger.rs` | No change — reused as-is via `Ledger<EscalationEvent>` |

---

### Task 1: Route data types in mur-common

**Files:**
- Create: `mur-common/src/route.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod route`)

- [ ] **Step 1: Write failing tests for RouteTier and RouteDecision**

Create `mur-common/src/route.rs` with only test code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_tier_serializes_as_lowercase() {
        let local = RouteTier::Local;
        let json = serde_json::to_string(&local).unwrap();
        assert_eq!(json, r#""local""#);

        let frontier = RouteTier::Frontier;
        let json = serde_json::to_string(&frontier).unwrap();
        assert_eq!(json, r#""frontier""#);
    }

    #[test]
    fn route_tier_deserializes_case_insensitive() {
        let tier: RouteTier = serde_json::from_str(r#""local""#).unwrap();
        assert_eq!(tier, RouteTier::Local);

        let tier: RouteTier = serde_json::from_str(r#""LOCAL""#).unwrap();
        assert_eq!(tier, RouteTier::Local);

        let tier: RouteTier = serde_json::from_str(r#""Frontier""#).unwrap();
        assert_eq!(tier, RouteTier::Frontier);
    }

    #[test]
    fn route_decision_display() {
        let d = RouteDecision::Local {
            model_id: "ollama_llama3".into(),
            reason: "low difficulty (score 0.15)".into(),
        };
        let s = d.to_string();
        assert!(s.contains("local"));
        assert!(s.contains("ollama_llama3"));

        let d = RouteDecision::Escalate {
            model_id: "anthropic_opus_4_7".into(),
            reason: "high complexity (score 0.82)".into(),
        };
        let s = d.to_string();
        assert!(s.contains("escalate"));
        assert!(s.contains("anthropic_opus_4_7"));
    }

    #[test]
    fn escalation_event_roundtrip() {
        let event = EscalationEvent {
            timestamp: "2026-06-01T12:00:00Z".into(),
            task_summary: "Refactor auth module".into(),
            difficulty_score: 0.82,
            task_type: TaskType::CodeGen,
            estimated_context_tokens: 3500,
            decision: RouteDecision::Escalate {
                model_id: "anthropic_opus_4_7".into(),
                reason: "high complexity".into(),
            },
            role: Some("dev".into()),
            escalation_from: Some("ollama_llama3".into()),
            estimated_cost_usd: 0.0525,
            counterfactual_cost_usd: 0.0525,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EscalationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_summary, "Refactor auth module");
        assert_eq!(parsed.difficulty_score, 0.82);
        assert_eq!(parsed.task_type, TaskType::CodeGen);
        assert_eq!(parsed.counterfactual_cost_usd, 0.0525);
        assert!(matches!(
            parsed.decision,
            RouteDecision::Escalate { .. }
        ));
    }

    #[test]
    fn route_policy_serde_roundtrip() {
        let policy = RoutePolicy::Auto;
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, RoutePolicy::Auto);

        let policy = RoutePolicy::PreferLocal;
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, RoutePolicy::PreferLocal);

        let policy = RoutePolicy::ForceFrontier {
            model_id: "claude".into(),
        };
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            parsed,
            RoutePolicy::ForceFrontier {
                model_id: "claude".into()
            }
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common route::tests -- --nocapture`
Expected: COMPILE ERROR — `RouteTier`, `RouteDecision`, `EscalationEvent`, `TaskType`, `RoutePolicy` not defined

- [ ] **Step 3: Write minimal types to make tests pass**

Replace the file with the full module:

```rust
//! Route data types for the cost-router orchestrator.
//!
//! These types are shared between `mur-core` (router logic) and
//! `mur-agent-runtime` (future Phase 2 spawn decisions).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Whether a model is cheap/local or frontier/expensive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouteTier {
    /// Free or near-free local model (Ollama, llama.cpp, mlx).
    Local,
    /// Paid cloud frontier model (Claude, GPT, Gemini).
    Frontier,
}

/// Which model and tier to use for a sub-task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteDecision {
    /// Route to a cheap/local model.
    Local {
        model_id: String,
        reason: String,
    },
    /// Escalate to a frontier model.
    Escalate {
        model_id: String,
        reason: String,
    },
}

impl fmt::Display for RouteDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteDecision::Local { model_id, reason } => {
                write!(f, "local → {model_id} ({reason})")
            }
            RouteDecision::Escalate { model_id, reason } => {
                write!(f, "escalate → {model_id} ({reason})")
            }
        }
    }
}

/// Categories of sub-tasks for difficulty scoring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    /// Writing or modifying code.
    CodeGen,
    /// Reviewing / auditing existing code.
    CodeReview,
    /// Searching / retrieving information.
    Retrieval,
    /// Refactoring across multiple files.
    Refactor,
    /// Writing or updating documentation.
    Documentation,
    /// Debugging / investigating issues.
    Debugging,
    /// Running tests or commands.
    Execution,
    /// General chat / Q&A.
    General,
}

impl TaskType {
    /// Base difficulty score 0.0–1.0 before other factors.
    pub fn base_difficulty(&self) -> f64 {
        match self {
            TaskType::CodeGen => 0.65,
            TaskType::Refactor => 0.70,
            TaskType::Debugging => 0.60,
            TaskType::CodeReview => 0.55,
            TaskType::Retrieval => 0.30,
            TaskType::Documentation => 0.35,
            TaskType::Execution => 0.25,
            TaskType::General => 0.40,
        }
    }
}

/// One routing decision recorded in the escalation audit ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationEvent {
    /// ISO-8601 timestamp.
    pub timestamp: String,
    /// Human-readable summary of what was asked.
    pub task_summary: String,
    /// 0.0–1.0 difficulty score from the heuristic.
    pub difficulty_score: f64,
    /// Classified task type.
    pub task_type: TaskType,
    /// Estimated context window tokens needed.
    pub estimated_context_tokens: u64,
    /// The routing decision made.
    pub decision: RouteDecision,
    /// Role that originated this task (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Which local model would have been used if not escalated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_from: Option<String>,
    /// Estimated USD cost of the route actually taken (0.0 for local).
    #[serde(default)]
    pub estimated_cost_usd: f64,
    /// Estimated USD this task would have cost on the frontier model —
    /// i.e. the cost avoided when routed local. Equals `estimated_cost_usd`
    /// for escalations.
    #[serde(default)]
    pub counterfactual_cost_usd: f64,
}

/// Per-role routing override, stored in `RoleEntry.route_policy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutePolicy {
    /// Let the heuristic decide (default).
    Auto,
    /// Bias toward local models; only escalate above a higher threshold.
    PreferLocal,
    /// Always use local models for this role.
    ForceLocal,
    /// Always use a specific frontier model for this role.
    ForceFrontier {
        /// Model registry key (e.g. "anthropic_opus_4_7").
        model_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_tier_serializes_as_lowercase() {
        let local = RouteTier::Local;
        let json = serde_json::to_string(&local).unwrap();
        assert_eq!(json, r#""local""#);

        let frontier = RouteTier::Frontier;
        let json = serde_json::to_string(&frontier).unwrap();
        assert_eq!(json, r#""frontier""#);
    }

    #[test]
    fn route_tier_deserializes_case_insensitive() {
        let tier: RouteTier = serde_json::from_str(r#""local""#).unwrap();
        assert_eq!(tier, RouteTier::Local);

        let tier: RouteTier = serde_json::from_str(r#""LOCAL""#).unwrap();
        assert_eq!(tier, RouteTier::Local);

        let tier: RouteTier = serde_json::from_str(r#""Frontier""#).unwrap();
        assert_eq!(tier, RouteTier::Frontier);
    }

    #[test]
    fn route_decision_display() {
        let d = RouteDecision::Local {
            model_id: "ollama_llama3".into(),
            reason: "low difficulty (score 0.15)".into(),
        };
        let s = d.to_string();
        assert!(s.contains("local"));
        assert!(s.contains("ollama_llama3"));

        let d = RouteDecision::Escalate {
            model_id: "anthropic_opus_4_7".into(),
            reason: "high complexity (score 0.82)".into(),
        };
        let s = d.to_string();
        assert!(s.contains("escalate"));
        assert!(s.contains("anthropic_opus_4_7"));
    }

    #[test]
    fn escalation_event_roundtrip() {
        let event = EscalationEvent {
            timestamp: "2026-06-01T12:00:00Z".into(),
            task_summary: "Refactor auth module".into(),
            difficulty_score: 0.82,
            task_type: TaskType::CodeGen,
            estimated_context_tokens: 3500,
            decision: RouteDecision::Escalate {
                model_id: "anthropic_opus_4_7".into(),
                reason: "high complexity".into(),
            },
            role: Some("dev".into()),
            escalation_from: Some("ollama_llama3".into()),
            estimated_cost_usd: 0.0525,
            counterfactual_cost_usd: 0.0525,
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: EscalationEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.task_summary, "Refactor auth module");
        assert_eq!(parsed.difficulty_score, 0.82);
        assert_eq!(parsed.task_type, TaskType::CodeGen);
        assert_eq!(parsed.counterfactual_cost_usd, 0.0525);
        assert!(matches!(
            parsed.decision,
            RouteDecision::Escalate { .. }
        ));
    }

    #[test]
    fn route_policy_serde_roundtrip() {
        let policy = RoutePolicy::Auto;
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, RoutePolicy::Auto);

        let policy = RoutePolicy::PreferLocal;
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(parsed, RoutePolicy::PreferLocal);

        let policy = RoutePolicy::ForceFrontier {
            model_id: "claude".into(),
        };
        let yaml = serde_yaml_ng::to_string(&policy).unwrap();
        let parsed: RoutePolicy = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(
            parsed,
            RoutePolicy::ForceFrontier {
                model_id: "claude".into()
            }
        );
    }

    #[test]
    fn task_type_base_difficulty_ordering() {
        // Execution and retrieval should be easiest.
        assert!(TaskType::Execution.base_difficulty() < TaskType::CodeGen.base_difficulty());
        assert!(TaskType::Retrieval.base_difficulty() < TaskType::CodeGen.base_difficulty());
        // Refactor should be hardest.
        assert!(TaskType::Refactor.base_difficulty() > TaskType::Documentation.base_difficulty());
        // All scores in [0.0, 1.0].
        let all = [
            TaskType::CodeGen,
            TaskType::CodeReview,
            TaskType::Retrieval,
            TaskType::Refactor,
            TaskType::Documentation,
            TaskType::Debugging,
            TaskType::Execution,
            TaskType::General,
        ];
        for tt in &all {
            let s = tt.base_difficulty();
            assert!((0.0..=1.0).contains(&s), "{tt:?} base_difficulty {s} out of range");
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-common route::tests`
Expected: All 6 tests PASS

- [ ] **Step 5: Add pub mod route to mur-common/src/lib.rs**

Alongside the other `pub mod` declarations (e.g. after `pub mod schedule_claim;`), insert:

```rust
pub mod route;
```

Verify it compiles: `cargo build -p mur-common`

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/route.rs mur-common/src/lib.rs
git commit -m "feat(route): add RouteTier, RouteDecision, EscalationEvent, RoutePolicy types

Phase 1 types for the cost-router orchestrator. Pure data types —
no logic yet. RouteTier classifies models as local or frontier.
RouteDecision captures the router's output. EscalationEvent is the
audit-ledger record. RoutePolicy encodes per-role routing overrides.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: Extend ModelEntry and RoleEntry with routing fields

**Files:**
- Modify: `mur-common/src/model.rs`

- [ ] **Step 1: Write failing tests for the new fields**

Add to the existing `tests` module in `mur-common/src/model.rs` (before the closing `}` of `mod tests`):

```rust
#[test]
fn model_entry_parses_tier_field() {
    let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
    tier: local
  opus:
    provider: anthropic
    model: claude-opus-4-7
    tier: frontier
    cost_per_1k_tokens: 0.015
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(r.models["haiku"].tier, Some(RouteTier::Local));
    assert_eq!(r.models["opus"].tier, Some(RouteTier::Frontier));
    assert_eq!(r.models["opus"].cost_per_1k_tokens, Some(0.015));
    // Missing tier is None.
    let mut r2 = ModelRegistry::default();
    r2.models.insert(
        "x".into(),
        ModelEntry {
            provider: "ollama".into(),
            model: "llama3".into(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
        },
    );
    let yaml = serde_yaml_ng::to_string(&r2).unwrap();
    assert!(!yaml.contains("tier:"), "absent tier should not be serialized: {yaml}");
}

#[test]
fn role_entry_parses_route_policy() {
    let yaml = r#"
schema_version: 1
models:
  haiku:
    provider: anthropic
    model: claude-haiku-4-5
  opus:
    provider: anthropic
    model: claude-opus-4-7
roles:
  dev:
    primary: opus
    route_policy:
      force_frontier:
        model_id: opus
  reflector:
    primary: haiku
    route_policy: prefer_local
  curator:
    primary: haiku
    route_policy: force_local
  chat:
    primary: haiku
"#;
    let r: ModelRegistry = serde_yaml_ng::from_str(yaml).unwrap();
    assert_eq!(
        r.roles["dev"].route_policy,
        Some(RoutePolicy::ForceFrontier {
            model_id: "opus".into()
        })
    );
    assert_eq!(r.roles["reflector"].route_policy, Some(RoutePolicy::PreferLocal));
    assert_eq!(r.roles["curator"].route_policy, Some(RoutePolicy::ForceLocal));
    assert_eq!(r.roles["chat"].route_policy, None);
}
```

Also add the necessary import at the top of the test module (inside `mod tests`):

```rust
use crate::route::{RoutePolicy, RouteTier};
```

Wait — we need this import at the file level. Add to the existing `use` block at the top of `model.rs` (alongside `use crate::secret::SecretRef;`):

```rust
use crate::route::{RoutePolicy, RouteTier};
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common model::tests::model_entry_parses_tier_field`
Expected: COMPILE ERROR — `ModelEntry` has no field `tier`, `RoleEntry` has no field `route_policy`

- [ ] **Step 3: Add fields to ModelEntry and RoleEntry**

In `ModelEntry` (line 21), add two fields after `pub params: serde_json::Value`:

```rust
    /// Routing tier: cheap/local vs frontier/expensive.
    /// When absent, the router infers based on provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<RouteTier>,
    /// Estimated USD cost per 1000 output tokens.
    /// Used for ledger cost estimates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_tokens: Option<f64>,
```

In `RoleEntry` (line 35), add one field after `pub privacy_local_only: bool`:

```rust
    /// Per-role routing policy override.
    /// When absent, the router uses the default heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_policy: Option<RoutePolicy>,
```

- [ ] **Step 4: Update existing struct literals across the workspace (compile fix)**

`ModelEntry` does **not** derive `Default`, so adding two new fields breaks **every**
struct-literal that builds it — `cargo build`/`cargo test` will fail with
`error[E0063]: missing fields tier, cost_per_1k_tokens in initializer of ModelEntry`
until all sites are updated. (`RoleEntry` *does* derive `Default`; its `mur-common`
literals already use `..Default::default()` and need no change, but the one full literal
in `cmd/model.rs` must be patched.)

Add `tier: None,` and `cost_per_1k_tokens: None,` immediately after the `params: ...` line
at all **6** `ModelEntry` literals in `mur-core` (4 production, 2 test — `cargo test`
compiles both):

| File | Location | Note |
|------|----------|------|
| `mur-core/src/cmd/agent/model_resolve.rs` | `apply_model_choice` (~76) | production |
| `mur-core/src/cmd/model.rs` | `Add` handler (~93) | production — **Task 7 Step 4 later replaces these `None`s** with `--tier`/`--cost-per-1k` parsing |
| `mur-core/src/cmd/model.rs` | `cmd_migrate` `or_insert_with` (~160) | production |
| `mur-core/src/cmd/fleet_sync.rs` | test (~565) | test |
| `mur-core/src/cmd/agent/install.rs` | test (~280) | test |
| `mur-core/src/cmd/agent/export.rs` | test (~204) | test |

Each is the same two-line addition, e.g. in `model_resolve.rs`:

```rust
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
        },
```

Then add `route_policy: None,` to the **1** full `RoleEntry` literal — the `RoleSubCmd::Set`
handler in `mur-core/src/cmd/model.rs` (~210). **Task 7 Step 2 later replaces this** with the
parsed policy:

```rust
                RoleEntry {
                    primary: model.clone(),
                    fallback,
                    cost_budget_per_day_usd: budget,
                    privacy_local_only,
                    route_policy: None,
                },
```

- [ ] **Step 5: Build the whole workspace to verify no site was missed**

The new fields cross both crates, so the compiler is the source of truth for missed literals:

Run: `cargo test --workspace --no-run`
Expected: Compiles cleanly. Any missed `ModelEntry`/`RoleEntry` literal surfaces as
`error[E0063]: missing fields …`.

Then run the new + existing model tests for regressions:
Run: `cargo test -p mur-common model::tests`
Expected: All model tests PASS (new `tier`/`route_policy` tests + existing round-trip,
resolve_role, etc.)

- [ ] **Step 6: Commit**

This commit spans both crates so the tree compiles at every step (bisect-safe):

```bash
git add mur-common/src/model.rs \
  mur-core/src/cmd/agent/model_resolve.rs \
  mur-core/src/cmd/agent/install.rs \
  mur-core/src/cmd/agent/export.rs \
  mur-core/src/cmd/fleet_sync.rs \
  mur-core/src/cmd/model.rs
git commit -m "feat(route): add tier/cost fields to ModelEntry and route_policy to RoleEntry

ModelEntry gains optional \`tier\` (Local/Frontier) and
\`cost_per_1k_tokens\` for ledger cost estimates. RoleEntry gains
optional \`route_policy\` for per-role routing overrides. ModelEntry has
no Default derive, so existing literals across mur-core are updated with
None defaults to keep the workspace compiling.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: Difficulty heuristic

**Files:**
- Create: `mur-core/src/route/heuristic.rs`
- Create: `mur-core/src/route/mod.rs` (minimal, just `pub mod heuristic;` for now)

- [ ] **Step 1: Write failing integration test for the heuristic**

Create `mur-core/tests/route_heuristic.rs`:

```rust
use mur_common::route::TaskType;

// We'll import the heuristic once it exists.
// mur_core::route::heuristic::DefaultHeuristic

#[test]
fn heuristic_scores_low_for_execution() {
    // This test will fail to compile until the module exists.
    // We use a placeholder that describes the expected behavior.
    //
    // let h = DefaultHeuristic::default();
    // let score = h.score(
    //     "run cargo test",
    //     TaskType::Execution,
    //     200,  // estimated tokens
    // );
    // assert!(score < 0.5, "execution should score low, got {score}");
    todo!("compile guard — replace with real test after Task 3 Step 3")
}

#[test]
fn heuristic_scores_high_for_refactor() {
    // let h = DefaultHeuristic::default();
    // let score = h.score(
    //     "refactor the auth module to use the new token format across all handlers",
    //     TaskType::Refactor,
    //     5000,
    // );
    // assert!(score > 0.5, "refactor should score high, got {score}");
    todo!("compile guard — replace with real test after Task 3 Step 3")
}

#[test]
fn heuristic_scores_higher_for_more_tokens() {
    // let h = DefaultHeuristic::default();
    // let small = h.score("fix typo in README", TaskType::Documentation, 100);
    // let large = h.score("fix typo in README", TaskType::Documentation, 10000);
    // assert!(large > small, "more tokens should increase score");
    todo!("compile guard — replace with real test after Task 3 Step 3")
}
```

Run: `cargo test -p mur-core route_heuristic -- --nocapture`
Expected: COMPILE ERROR — module not found

- [ ] **Step 2: Run to confirm it fails**

Already confirmed above — module doesn't exist yet.

- [ ] **Step 3: Write the DefaultHeuristic implementation**

Create `mur-core/src/route/heuristic.rs`:

```rust
//! Difficulty heuristic for the cost-router.
//!
//! Scores sub-tasks 0.0–1.0 based on task type, context size, and
//! keyword signals. The router uses this score to decide local vs
//! frontier routing.

use mur_common::route::TaskType;

/// Scores a sub-task's difficulty 0.0 (trivial) to 1.0 (needs frontier).
pub trait DifficultyHeuristic {
    /// Compute a difficulty score.
    ///
    /// * `task_summary` — human-readable description of the work.
    /// * `task_type` — classified task category.
    /// * `estimated_tokens` — rough context-window size estimate.
    fn score(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
    ) -> f64;
}

/// Default difficulty heuristic.
///
/// Combines:
/// 1. Base score from `TaskType::base_difficulty()` (`WEIGHT_BASE`)
/// 2. Context-size factor (`WEIGHT_CONTEXT`)
/// 3. Keyword boost (`WEIGHT_KEYWORD`)
///
/// The three weights sum to 1.0, so each factor in [0,1] yields a score in
/// [0.0, 1.0] (clamped). Realistic max ≈ 0.85 (highest `base_difficulty` is
/// `Refactor` at 0.70), which keeps `PREFER_LOCAL_THRESHOLD` (0.75) reachable.
#[derive(Debug, Clone)]
pub struct DefaultHeuristic {
    /// Tokens at which the context factor hits 1.0.
    pub max_context_tokens: u64,
    /// Tokens below which context contribution is ~0.
    pub min_context_tokens: u64,
}

impl Default for DefaultHeuristic {
    fn default() -> Self {
        Self {
            max_context_tokens: 100_000,
            min_context_tokens: 100,
        }
    }
}

/// Weight of the task-type base difficulty in the final score.
const WEIGHT_BASE: f64 = 0.50;
/// Weight of the log-scale context-size factor.
const WEIGHT_CONTEXT: f64 = 0.35;
/// Weight of the keyword-complexity boost.
const WEIGHT_KEYWORD: f64 = 0.15;
/// Number of high-complexity keyword matches that saturate the boost at 1.0.
/// (Invariant: `WEIGHT_BASE + WEIGHT_CONTEXT + WEIGHT_KEYWORD == 1.0`.)
const KEYWORD_SATURATION: usize = 3;

impl DefaultHeuristic {
    /// Keywords that signal high complexity and boost the score.
    const HIGH_COMPLEXITY_KEYWORDS: &'static [&'static str] = &[
        "refactor",
        "rewrite",
        "redesign",
        "architect",
        "migrate",
        "multi-file",
        "cross-cutting",
        "breaking change",
        "backward compat",
        "concurrency",
        "race condition",
        "deadlock",
        "security audit",
        "vulnerability",
        "optimize",
        "performance critical",
    ];

    /// Normalized [0.0, 1.0] keyword signal: rises linearly with the number of
    /// high-complexity keywords present, saturating at `KEYWORD_SATURATION`.
    /// `score` multiplies this by `WEIGHT_KEYWORD`.
    fn keyword_boost(&self, summary: &str) -> f64 {
        let lower = summary.to_lowercase();
        let matches = Self::HIGH_COMPLEXITY_KEYWORDS
            .iter()
            .filter(|kw| lower.contains(&kw.to_lowercase()))
            .count();
        (matches as f64 / KEYWORD_SATURATION as f64).min(1.0)
    }

    fn context_factor(&self, estimated_tokens: u64) -> f64 {
        if estimated_tokens <= self.min_context_tokens {
            return 0.0;
        }
        if estimated_tokens >= self.max_context_tokens {
            return 1.0;
        }
        // Log-scale interpolation so the curve rises quickly at first then
        // flattens.  Small tasks stay cheap; large tasks ramp toward 1.0.
        let log_min = (self.min_context_tokens as f64).ln();
        let log_max = (self.max_context_tokens as f64).ln();
        let log_val = (estimated_tokens as f64).ln();
        let raw = (log_val - log_min) / (log_max - log_min);
        raw.clamp(0.0, 1.0)
    }
}

impl DifficultyHeuristic for DefaultHeuristic {
    fn score(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
    ) -> f64 {
        let base = task_type.base_difficulty();
        let context = self.context_factor(estimated_tokens);
        let keywords = self.keyword_boost(task_summary);

        let raw = WEIGHT_BASE * base + WEIGHT_CONTEXT * context + WEIGHT_KEYWORD * keywords;
        raw.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_scores_low() {
        let h = DefaultHeuristic::default();
        let score = h.score("run cargo test", TaskType::Execution, 200);
        assert!(score < 0.5, "execution should score low, got {score}");
    }

    #[test]
    fn refactor_scores_high() {
        let h = DefaultHeuristic::default();
        let score = h.score(
            "refactor the auth module to use the new token format",
            TaskType::Refactor,
            5000,
        );
        assert!(score > 0.5, "refactor should score high, got {score}");
    }

    #[test]
    fn more_tokens_increases_score() {
        let h = DefaultHeuristic::default();
        let small = h.score("fix typo in README", TaskType::Documentation, 100);
        let large = h.score("fix typo in README", TaskType::Documentation, 10000);
        assert!(large > small, "more tokens should increase score: {small} vs {large}");
    }

    #[test]
    fn keyword_boost_works() {
        let h = DefaultHeuristic::default();
        let without = h.score("change the color", TaskType::CodeGen, 500);
        let with = h.score("refactor and rewrite the auth module", TaskType::CodeGen, 500);
        assert!(with > without, "keywords should boost: {without} vs {with}");
    }

    #[test]
    fn score_clamped_to_one() {
        let h = DefaultHeuristic::default();
        let score = h.score(
            "redesign architecture migrate security audit",
            TaskType::Refactor,
            200_000,
        );
        assert!((0.0..=1.0).contains(&score), "score {score} out of range");
    }

    #[test]
    fn context_factor_log_scale() {
        let h = DefaultHeuristic::default();
        // 100 tokens → ~0, 100k tokens → ~1.0
        let low = h.context_factor(100);
        let mid = h.context_factor(10_000);
        let high = h.context_factor(100_000);
        assert!(low < 0.1, "low={low}");
        assert!(mid > 0.4, "mid={mid}");
        assert!(high > 0.95, "high={high}");
        assert!(mid - low > 0.2, "log scale should give separation");
    }
}
```

Create `mur-core/src/route/mod.rs`:

```rust
//! Cost-router orchestrator — difficulty heuristic, routing decisions,
//! and escalation audit ledger.
//!
//! Phase 1 (this module): route decisions + audit ledger.
//! Phase 2 (deferred): governed spawn via `CodingAgentAdapter`.

pub mod heuristic;
```

- [ ] **Step 4: Update the integration test**

Replace `mur-core/tests/route_heuristic.rs` with actual test code:

```rust
use mur_common::route::TaskType;
use mur_core::route::heuristic::{DefaultHeuristic, DifficultyHeuristic};

#[test]
fn heuristic_scores_low_for_execution() {
    let h = DefaultHeuristic::default();
    let score = h.score("run cargo test", TaskType::Execution, 200);
    assert!(score < 0.5, "execution should score low, got {score}");
}

#[test]
fn heuristic_scores_high_for_refactor() {
    let h = DefaultHeuristic::default();
    let score = h.score(
        "refactor the auth module to use the new token format across all handlers",
        TaskType::Refactor,
        5000,
    );
    assert!(score > 0.5, "refactor should score high, got {score}");
}

#[test]
fn heuristic_scores_higher_for_more_tokens() {
    let h = DefaultHeuristic::default();
    let small = h.score("fix typo in README", TaskType::Documentation, 100);
    let large = h.score("fix typo in README", TaskType::Documentation, 10000);
    assert!(large > small, "more tokens should increase score: {small} vs {large}");
}
```

Run unit tests: `cargo test -p mur-core route::heuristic::tests`
Expected: 6 tests PASS

Run integration tests: `cargo test -p mur-core route_heuristic`
Expected: 3 tests PASS

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/route/mod.rs mur-core/src/route/heuristic.rs mur-core/tests/route_heuristic.rs
git commit -m "feat(route): add DefaultHeuristic difficulty scorer

Scores sub-tasks 0.0-1.0 combining task-type base (50%), log-scale
context-size factor (35%), and keyword boost (15%). Used by the
Router to decide local vs. frontier routing.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: Router — combine heuristic + registry + overrides

**Files:**
- Modify: `mur-core/src/route/mod.rs` (add `Router` struct)
- Create: `mur-core/tests/common/mod.rs` (shared integration-test fixtures, reused by Tasks 5 & 8)
- Create: `mur-core/tests/route_router.rs`

- [ ] **Step 1: Add the shared fixture, then write a genuinely failing test**

A `common/` subdirectory under `tests/` is **not** compiled as its own test
binary, so it is the idiomatic place to share fixtures across the `route_*`
integration tests (reused by Tasks 5 & 8). Create `mur-core/tests/common/mod.rs`:

```rust
//! Shared fixtures for the route integration tests.
//! `#![allow(dead_code)]` because each integration binary uses only a subset,
//! and Rust lints unused items per-binary (`-D warnings` would otherwise fail).
#![allow(dead_code)]

use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::{EscalationEvent, RouteDecision, RouteTier, TaskType};

/// A registry with one local and one (priced) frontier model.
pub fn test_registry() -> ModelRegistry {
    let mut reg = ModelRegistry::default();
    reg.models.insert(
        "ollama_llama3".into(),
        ModelEntry {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
            capabilities: vec!["chat".into()],
            params: serde_json::Value::Null,
            tier: Some(RouteTier::Local),
            cost_per_1k_tokens: None,
        },
    );
    reg.models.insert(
        "anthropic_opus".into(),
        ModelEntry {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            base_url: None,
            secret: None,
            capabilities: vec!["chat".into(), "tools".into()],
            params: serde_json::Value::Null,
            tier: Some(RouteTier::Frontier),
            cost_per_1k_tokens: Some(0.015),
        },
    );
    reg
}

/// A canned local/escalation audit event for ledger tests.
/// (1000 tokens × $0.015/1k = $0.015 frontier cost, so the numbers are
/// self-consistent with `Router::audit`.)
pub fn make_event(escalate: bool) -> EscalationEvent {
    EscalationEvent {
        timestamp: "2026-06-01T12:00:00Z".into(),
        task_summary: "test task".into(),
        difficulty_score: if escalate { 0.82 } else { 0.15 },
        task_type: TaskType::General,
        estimated_context_tokens: 1000,
        decision: if escalate {
            RouteDecision::Escalate {
                model_id: "anthropic_opus".into(),
                reason: "high difficulty".into(),
            }
        } else {
            RouteDecision::Local {
                model_id: "ollama_llama3".into(),
                reason: "low difficulty".into(),
            }
        },
        role: None,
        escalation_from: if escalate { Some("ollama_llama3".into()) } else { None },
        estimated_cost_usd: if escalate { 0.015 } else { 0.0 },
        counterfactual_cost_usd: 0.015,
    }
}
```

Then create `mur-core/tests/route_router.rs` with **real** assertions against the
not-yet-existent `Router` — a true red that fails to **compile** (no `todo!()`
that would compile and panic):

```rust
mod common;
use common::test_registry;
use mur_common::model::RoleEntry;
use mur_common::route::{RouteDecision, RoutePolicy, TaskType};
use mur_core::route::Router;

#[test]
fn easy_task_routes_to_local() {
    let router = Router::new(test_registry()).unwrap();
    let decision = router.decide("run cargo fmt", TaskType::Execution, 200, None);
    assert!(matches!(decision, RouteDecision::Local { .. }), "got {decision:?}");
}

#[test]
fn hard_task_routes_to_frontier() {
    let router = Router::new(test_registry()).unwrap();
    let decision = router.decide(
        "refactor the entire auth system across 12 modules",
        TaskType::Refactor,
        8000,
        None,
    );
    assert!(matches!(decision, RouteDecision::Escalate { .. }), "got {decision:?}");
}

#[test]
fn force_local_override_wins() {
    let mut reg = test_registry();
    reg.roles.insert(
        "reflector".into(),
        RoleEntry {
            primary: "ollama_llama3".into(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: Some(RoutePolicy::ForceLocal),
        },
    );
    let router = Router::new(reg).unwrap();
    let decision =
        router.decide("refactor everything", TaskType::Refactor, 10_000, Some("reflector"));
    assert!(matches!(decision, RouteDecision::Local { .. }), "got {decision:?}");
}

#[test]
fn force_frontier_trumps_easy_task() {
    let mut reg = test_registry();
    reg.roles.insert(
        "dev".into(),
        RoleEntry {
            primary: "anthropic_opus".into(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: Some(RoutePolicy::ForceFrontier {
                model_id: "anthropic_opus".into(),
            }),
        },
    );
    let router = Router::new(reg).unwrap();
    let decision = router.decide("echo hello", TaskType::Execution, 50, Some("dev"));
    assert!(matches!(decision, RouteDecision::Escalate { .. }), "got {decision:?}");
}

#[test]
fn decide_with_score_exposes_score() {
    let router = Router::new(test_registry()).unwrap();
    let (_decision, score) =
        router.decide_with_score("medium task", TaskType::CodeGen, 500, None);
    assert!((0.0..=1.0).contains(&score));
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p mur-core route_router`
Expected: COMPILE ERROR — `mur_core::route::Router` does not exist yet (genuine red).

- [ ] **Step 3: Implement Router struct**

Add to `mur-core/src/route/mod.rs` (after the `pub mod heuristic;` line):

```rust
use anyhow::Result;
use mur_common::model::{ModelEntry, ModelRegistry};
use mur_common::route::{EscalationEvent, RouteDecision, RoutePolicy, RouteTier, TaskType};

use crate::route::heuristic::{DefaultHeuristic, DifficultyHeuristic};

/// Default difficulty threshold above which a task escalates to frontier.
const DEFAULT_ESCALATION_THRESHOLD: f64 = 0.55;

/// Higher escalation threshold applied when a role's policy is `PreferLocal`.
/// Reachable because the heuristic's realistic max is ≈ 0.85 (see
/// `DefaultHeuristic`).
const PREFER_LOCAL_THRESHOLD: f64 = 0.75;

/// Providers treated as local/cheap when a model carries no explicit `tier`.
const LOCAL_PROVIDERS: &[&str] =
    &["ollama", "llamacpp", "llama_cpp", "mlx", "lmstudio", "local"];

/// Enforced by `Router::new` (which rejects an empty registry); justifies the
/// `expect` on cross-tier degradation, where at least one model always exists.
const REGISTRY_NONEMPTY_INVARIANT: &str =
    "Router::new guarantees a non-empty registry, so a model always exists";

/// Combines the difficulty heuristic with the model registry and per-role
/// overrides to decide local vs. frontier routing for a sub-task.
pub struct Router {
    registry: ModelRegistry,
    heuristic: DefaultHeuristic,
    escalation_threshold: f64,
}

impl Router {
    /// Create a new Router from a model registry.
    ///
    /// Errors if the registry has no models — routing cannot pick a target
    /// otherwise. A single-tier registry is fine: tasks degrade gracefully to
    /// whatever tier exists.
    pub fn new(registry: ModelRegistry) -> Result<Self> {
        if registry.models.is_empty() {
            anyhow::bail!(
                "cannot build a Router: the model registry is empty — add at \
                 least one model with `mur model add`"
            );
        }
        Ok(Self {
            registry,
            heuristic: DefaultHeuristic::default(),
            escalation_threshold: DEFAULT_ESCALATION_THRESHOLD,
        })
    }

    /// Decide where to route a sub-task.
    ///
    /// Thin wrapper over [`Router::decide_with_score`] that discards the score.
    /// There is exactly **one** routing code path (`decide_with_score`), so the
    /// two entry points can never disagree.
    pub fn decide(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
        role: Option<&str>,
    ) -> RouteDecision {
        self.decide_with_score(task_summary, task_type, estimated_tokens, role)
            .0
    }

    /// Decide where to route a sub-task, returning the decision **and** the
    /// difficulty score (recorded in the escalation ledger).
    ///
    /// Single source of truth for routing — [`Router::decide`] and
    /// [`Router::audit`] both delegate here. Order of precedence:
    /// 1. Per-role [`RoutePolicy`] override: `ForceLocal` / `ForceFrontier` /
    ///    `PreferLocal` (higher threshold); `Auto` falls through to the heuristic.
    /// 2. Difficulty score vs. the escalation threshold.
    pub fn decide_with_score(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
        role: Option<&str>,
    ) -> (RouteDecision, f64) {
        let score = self.heuristic.score(task_summary, task_type, estimated_tokens);

        // 1. Per-role policy override (Auto falls through to the heuristic).
        if let Some(role_name) = role
            && let Some(policy) = self.role_policy(role_name)
        {
            match policy {
                RoutePolicy::Auto => {}
                RoutePolicy::PreferLocal => {
                    return (
                        self.by_threshold(score, PREFER_LOCAL_THRESHOLD, "prefer-local"),
                        score,
                    );
                }
                RoutePolicy::ForceLocal => {
                    return (
                        RouteDecision::Local {
                            model_id: self.local_or_frontier(),
                            reason: "role policy: force_local".into(),
                        },
                        score,
                    );
                }
                RoutePolicy::ForceFrontier { model_id } => {
                    let id = if self.registry.models.contains_key(model_id) {
                        model_id.clone()
                    } else {
                        self.frontier_or_local()
                    };
                    return (
                        RouteDecision::Escalate {
                            model_id: id,
                            reason: format!("role policy: force_frontier → {model_id}"),
                        },
                        score,
                    );
                }
            }
        }

        // 2. No (or Auto) override — route by the default threshold.
        (
            self.by_threshold(score, self.escalation_threshold, "threshold"),
            score,
        )
    }

    /// Build a fully-populated audit event for a routing decision — including
    /// the counterfactual local model and cost estimates. `timestamp` is
    /// supplied by the caller (RFC-3339) so the Router stays deterministic and
    /// testable.
    pub fn audit(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
        role: Option<&str>,
        timestamp: &str,
    ) -> EscalationEvent {
        let (decision, score) =
            self.decide_with_score(task_summary, task_type, estimated_tokens, role);

        // Cost the frontier alternative: tokens × best-frontier price per 1k.
        let frontier_cost_per_1k = self.frontier_cost_per_1k().unwrap_or(0.0);
        let counterfactual = estimated_tokens as f64 / 1000.0 * frontier_cost_per_1k;

        let (estimated_cost, escalation_from) = match &decision {
            RouteDecision::Escalate { .. } => (counterfactual, self.pick_best_local()),
            RouteDecision::Local { .. } => (0.0, None),
        };

        EscalationEvent {
            timestamp: timestamp.to_string(),
            task_summary: task_summary.to_string(),
            difficulty_score: score,
            task_type,
            estimated_context_tokens: estimated_tokens,
            decision,
            role: role.map(str::to_string),
            escalation_from,
            estimated_cost_usd: estimated_cost,
            counterfactual_cost_usd: counterfactual,
        }
    }

    /// Route by a difficulty threshold, degrading across tiers when one tier is
    /// unregistered. `label` names the threshold in the reason string.
    fn by_threshold(&self, score: f64, threshold: f64, label: &str) -> RouteDecision {
        if score >= threshold {
            match self.pick_best_frontier() {
                Some(model_id) => RouteDecision::Escalate {
                    model_id,
                    reason: format!("difficulty {score:.2} ≥ {label} {threshold:.2}"),
                },
                None => RouteDecision::Local {
                    model_id: self.pick_best_local().expect(REGISTRY_NONEMPTY_INVARIANT),
                    reason: format!(
                        "difficulty {score:.2} ≥ {label}, but no frontier model — using local"
                    ),
                },
            }
        } else {
            match self.pick_best_local() {
                Some(model_id) => RouteDecision::Local {
                    model_id,
                    reason: format!("difficulty {score:.2} < {label} {threshold:.2}"),
                },
                None => RouteDecision::Escalate {
                    model_id: self.pick_best_frontier().expect(REGISTRY_NONEMPTY_INVARIANT),
                    reason: format!(
                        "difficulty {score:.2} < {label}, but no local model — using frontier"
                    ),
                },
            }
        }
    }

    /// Return the role's routing policy, if any.
    fn role_policy(&self, role_name: &str) -> Option<&RoutePolicy> {
        self.registry.roles.get(role_name)?.route_policy.as_ref()
    }

    /// Effective tier for a model: its explicit `tier`, or inferred from the
    /// provider when unset (honors the `ModelEntry.tier` doc contract).
    fn effective_tier(entry: &ModelEntry) -> RouteTier {
        if let Some(tier) = entry.tier {
            return tier;
        }
        if LOCAL_PROVIDERS.contains(&entry.provider.to_lowercase().as_str()) {
            RouteTier::Local
        } else {
            RouteTier::Frontier
        }
    }

    /// Pick the best model in `tier` by capability count. Ties resolve
    /// deterministically by the `BTreeMap`'s key order.
    fn pick_best(&self, tier: RouteTier) -> Option<String> {
        self.registry
            .models
            .iter()
            .filter(|(_, e)| Self::effective_tier(e) == tier)
            .max_by_key(|(_, e)| e.capabilities.len())
            .map(|(k, _)| k.clone())
    }

    fn pick_best_local(&self) -> Option<String> {
        self.pick_best(RouteTier::Local)
    }

    fn pick_best_frontier(&self) -> Option<String> {
        self.pick_best(RouteTier::Frontier)
    }

    /// Best local model, degrading to frontier if no local model exists.
    fn local_or_frontier(&self) -> String {
        self.pick_best_local()
            .or_else(|| self.pick_best_frontier())
            .expect(REGISTRY_NONEMPTY_INVARIANT)
    }

    /// Best frontier model, degrading to local if no frontier model exists.
    fn frontier_or_local(&self) -> String {
        self.pick_best_frontier()
            .or_else(|| self.pick_best_local())
            .expect(REGISTRY_NONEMPTY_INVARIANT)
    }

    /// USD-per-1k of the best frontier model, if known.
    fn frontier_cost_per_1k(&self) -> Option<f64> {
        let id = self.pick_best_frontier()?;
        self.registry.models.get(&id)?.cost_per_1k_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_registry() -> ModelRegistry {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "ollama_llama3".into(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3.2:3b".into(),
                base_url: None,
                secret: None,
                capabilities: vec!["chat".into()],
                params: serde_json::Value::Null,
                tier: Some(RouteTier::Local),
                cost_per_1k_tokens: None,
            },
        );
        reg.models.insert(
            "anthropic_opus".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                base_url: None,
                secret: None,
                capabilities: vec!["chat".into(), "tools".into()],
                params: serde_json::Value::Null,
                tier: Some(RouteTier::Frontier),
                cost_per_1k_tokens: Some(0.015),
            },
        );
        reg
    }

    #[test]
    fn easy_task_routes_to_local() {
        let router = Router::new(test_registry()).unwrap();
        let decision = router.decide("run cargo fmt", TaskType::Execution, 200, None);
        assert!(
            matches!(decision, RouteDecision::Local { .. }),
            "expected Local, got {decision:?}"
        );
    }

    #[test]
    fn hard_task_routes_to_frontier() {
        let router = Router::new(test_registry()).unwrap();
        let decision = router.decide(
            "refactor the entire auth system across 12 modules",
            TaskType::Refactor,
            8000,
            None,
        );
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "expected Escalate, got {decision:?}"
        );
    }

    #[test]
    fn force_local_override_wins() {
        let mut reg = test_registry();
        reg.roles.insert(
            "reflector".into(),
            RoleEntry {
                primary: "ollama_llama3".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::ForceLocal),
            },
        );
        let router = Router::new(reg).unwrap();
        let decision = router.decide(
            "refactor everything",
            TaskType::Refactor,
            10_000,
            Some("reflector"),
        );
        assert!(
            matches!(decision, RouteDecision::Local { .. }),
            "force_local should win, got {decision:?}"
        );
    }

    #[test]
    fn force_frontier_override_wins() {
        let mut reg = test_registry();
        reg.roles.insert(
            "dev".into(),
            RoleEntry {
                primary: "anthropic_opus".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::ForceFrontier {
                    model_id: "anthropic_opus".into(),
                }),
            },
        );
        let router = Router::new(reg).unwrap();
        let decision = router.decide(
            "run echo hello",
            TaskType::Execution,
            50,
            Some("dev"),
        );
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "force_frontier should win even on trivial tasks, got {decision:?}"
        );
    }

    #[test]
    fn no_local_model_escalates() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "anthropic_opus".into(),
            ModelEntry {
                provider: "anthropic".into(),
                model: "claude-opus-4-7".into(),
                base_url: None,
                secret: None,
                capabilities: vec!["chat".into()],
                params: serde_json::Value::Null,
                tier: Some(RouteTier::Frontier),
                cost_per_1k_tokens: Some(0.015),
            },
        );
        let router = Router::new(reg).unwrap();
        let decision = router.decide("echo hello", TaskType::Execution, 50, None);
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "no local model → must escalate, got {decision:?}"
        );
    }

    #[test]
    fn decide_with_score_returns_score() {
        let router = Router::new(test_registry()).unwrap();
        let (decision, score) = router.decide_with_score(
            "medium task",
            TaskType::CodeGen,
            500,
            None,
        );
        assert!((0.0..=1.0).contains(&score));
        // Medium code-gen task around the threshold — don't assert the
        // decision, just that we got a valid one.
        match &decision {
            RouteDecision::Local { model_id, .. } => {
                assert_eq!(model_id, "ollama_llama3");
            }
            RouteDecision::Escalate { model_id, .. } => {
                assert_eq!(model_id, "anthropic_opus");
            }
        }
    }

    #[test]
    fn prefer_local_raises_threshold() {
        let mut reg = test_registry();
        reg.roles.insert(
            "chat".into(),
            RoleEntry {
                primary: "ollama_llama3".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::PreferLocal),
            },
        );
        let router = Router::new(reg).unwrap();
        // A moderately-hard task that would normally escalate should stay local
        // under PreferLocal.
        let (decision, _score) = router.decide_with_score(
            "refactor a function",
            TaskType::Refactor,
            3000,
            Some("chat"),
        );
        // PreferLocal threshold is 0.75. Refactor base 0.70, ctx(3000) ≈ 0.49,
        // keywords (1 hit) ≈ 0.33 → 0.50·0.70 + 0.35·0.49 + 0.15·0.33 ≈ 0.57
        // < 0.75 → stays local.
        assert!(
            matches!(decision, RouteDecision::Local { .. }),
            "prefer_local should keep moderate tasks local, got {decision:?}"
        );
    }

    #[test]
    fn prefer_local_still_escalates_extreme_tasks() {
        // Reachability guard: the heuristic max is ≈ 0.85, so the 0.75
        // prefer-local threshold IS reachable — an extreme task must escalate.
        let mut reg = test_registry();
        reg.roles.insert(
            "chat".into(),
            RoleEntry {
                primary: "ollama_llama3".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::PreferLocal),
            },
        );
        let router = Router::new(reg).unwrap();
        // base 0.70 + ctx(200k)=1.0 + keywords (redesign/rewrite/migrate → 1.0)
        // → 0.50·0.70 + 0.35 + 0.15 = 0.85 ≥ 0.75 → escalate.
        let (decision, score) = router.decide_with_score(
            "redesign rewrite migrate the storage engine",
            TaskType::Refactor,
            200_000,
            Some("chat"),
        );
        assert!(score >= 0.75, "extreme task should exceed prefer-local threshold, got {score}");
        assert!(
            matches!(decision, RouteDecision::Escalate { .. }),
            "extreme task must escalate even under prefer_local, got {decision:?}"
        );
    }

    #[test]
    fn decide_matches_decide_with_score_for_auto_role() {
        // Regression: `decide()` must delegate to `decide_with_score()`. Previously
        // `decide()` handled an explicit `Auto` role by recursing with
        // `TaskType::General` and 0 tokens — discarding the real task type and
        // size — so a hard task routed Local via `decide()` but Escalate via
        // `decide_with_score()`. No test caught the divergence.
        let mut reg = test_registry();
        reg.roles.insert(
            "dev".into(),
            RoleEntry {
                primary: "anthropic_opus".into(),
                fallback: None,
                cost_budget_per_day_usd: None,
                privacy_local_only: false,
                route_policy: Some(RoutePolicy::Auto),
            },
        );
        let router = Router::new(reg).unwrap();
        // Hard task (Refactor base 0.70 + ctx(8000) → ~0.58 ≥ 0.55 threshold)
        // must escalate whether or not the role is explicitly `Auto`.
        let hard = "refactor the entire auth system across 12 modules";
        let via_decide = router.decide(hard, TaskType::Refactor, 8000, Some("dev"));
        let (via_score, _) =
            router.decide_with_score(hard, TaskType::Refactor, 8000, Some("dev"));
        assert_eq!(
            std::mem::discriminant(&via_decide),
            std::mem::discriminant(&via_score),
            "decide() and decide_with_score() must agree: {via_decide:?} vs {via_score:?}"
        );
        assert!(
            matches!(via_decide, RouteDecision::Escalate { .. }),
            "an explicit Auto role must still escalate a hard task, got {via_decide:?}"
        );
    }
}
```

- [ ] **Step 4: Verify unit + integration tests pass**

The integration tests were written in Step 1; they now compile against the real
`Router`. Run both suites:

```
cargo test -p mur-core route::router::tests
cargo test -p mur-core route_router
```
Expected: 9 unit tests PASS, 5 integration tests PASS

- [ ] **Step 5: Register the route module in mur-core/src/lib.rs**

Add alongside the other `pub mod` declarations (e.g. next to `pub mod retrieve;`):

```rust
pub mod route;
```

Build check: `cargo build -p mur-core`
Expected: Compiles cleanly

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/route/mod.rs mur-core/src/lib.rs mur-core/tests/common/mod.rs mur-core/tests/route_router.rs
git commit -m "feat(route): add Router combining heuristic + model registry + overrides

Router::decide() scores a sub-task with DefaultHeuristic, checks
per-role RoutePolicy overrides (ForceLocal, ForceFrontier, PreferLocal),
and returns a RouteDecision::Local or RouteDecision::Escalate.
decide_with_score() also exposes the difficulty score for the audit ledger.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: Escalation audit ledger

**Files:**
- Create: `mur-core/src/route/ledger.rs`

- [ ] **Step 1: Write a genuinely failing test for EscalationLedger**

Create `mur-core/tests/route_ledger.rs` with a real test against the
not-yet-existent `EscalationLedger` — a true compile-red, not a `todo!()`. It
reuses the shared `make_event` fixture from Task 4's `tests/common/mod.rs`:

```rust
mod common;
use common::make_event;
use mur_core::route::ledger::EscalationLedger;
use tempfile::TempDir;

#[test]
fn ledger_records_escalation_event() {
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
    ledger.append(&make_event(true)).unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let events = EscalationLedger::replay_today(tmp.path());
    assert_eq!(events.len(), 1);
}
```

- [ ] **Step 2: Run to confirm it fails**

Run: `cargo test -p mur-core route_ledger`
Expected: COMPILE ERROR — `mur_core::route::ledger::EscalationLedger` does not exist yet.

- [ ] **Step 3: Implement EscalationLedger**

Create `mur-core/src/route/ledger.rs`:

```rust
//! Escalation audit ledger for the cost-router orchestrator.
//!
//! Wraps `mur_common::ledger::Ledger<EscalationEvent>` with a
//! domain-specific API. Stored at `~/.mur/route/ledger/YYYY-MM-DD.jsonl`.
//!
//! This is the cost-visibility surface: every escalation decision is
//! recorded so the savings thesis is measurable before Phase 2 (spawn).

use mur_common::ledger::Ledger as GenericLedger;
use mur_common::route::EscalationEvent;
use std::path::Path;

/// Ledger for escalation routing decisions.
///
/// One JSONL record per routing decision that either escalated to frontier
/// or would-have-escalated. The ledger lives at `~/.mur/route/ledger/`.
pub struct EscalationLedger {
    inner: GenericLedger<EscalationEvent>,
}

impl EscalationLedger {
    /// Open (or create) the escalation ledger directory.
    /// Default path: `~/.mur/route/ledger/`.
    pub fn open(base_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: GenericLedger::open(base_dir)?,
        })
    }

    /// Open the ledger at the default path (`~/.mur/route/ledger/`).
    pub fn open_default() -> anyhow::Result<Self> {
        // Reuse the canonical root resolver instead of re-implementing it.
        let path = crate::paths::mur_root(None).join("route").join("ledger");
        Self::open(&path)
    }

    /// Append one escalation event to today's JSONL file.
    pub fn append(&mut self, event: &EscalationEvent) -> anyhow::Result<()> {
        self.inner.append(event)
    }

    /// Flush pending writes to disk.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.inner.flush()
    }

    /// Replay today's ledger events.
    pub fn replay_today(base_dir: &Path) -> Vec<EscalationEvent> {
        GenericLedger::<EscalationEvent>::scan_days(base_dir, 1)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Replay the last `days` of ledger events.
    pub fn replay_days(base_dir: &Path, days: u32) -> Vec<EscalationEvent> {
        GenericLedger::<EscalationEvent>::scan_days(base_dir, days)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Count escalation rate over the last `days`.
    /// Returns (escalations, total_decisions, rate).
    pub fn escalation_rate(base_dir: &Path, days: u32) -> (usize, usize, f64) {
        let s = Self::summary(base_dir, days);
        (s.escalations, s.total, s.rate)
    }

    /// Aggregate escalation **and cost** KPIs over the last `days`. This is the
    /// savings surface: `savings_usd` is the money avoided by routing cheap
    /// tasks locally instead of escalating everything to frontier.
    pub fn summary(base_dir: &Path, days: u32) -> LedgerSummary {
        let events = Self::replay_days(base_dir, days);
        let total = events.len();
        let mut escalations = 0;
        let mut spend_usd = 0.0;
        let mut savings_usd = 0.0;
        for e in &events {
            if matches!(e.decision, mur_common::route::RouteDecision::Escalate { .. }) {
                escalations += 1;
            }
            spend_usd += e.estimated_cost_usd;
            // Money avoided on tasks that stayed local.
            savings_usd += (e.counterfactual_cost_usd - e.estimated_cost_usd).max(0.0);
        }
        let rate = if total > 0 {
            escalations as f64 / total as f64
        } else {
            0.0
        };
        LedgerSummary {
            escalations,
            total,
            rate,
            spend_usd,
            savings_usd,
        }
    }
}

/// Aggregate escalation + cost KPIs over a window of ledger events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgerSummary {
    /// Decisions that escalated to a frontier model.
    pub escalations: usize,
    /// Total routing decisions recorded.
    pub total: usize,
    /// `escalations / total` (0.0 when empty).
    pub rate: f64,
    /// Estimated USD actually spent on frontier escalations.
    pub spend_usd: f64,
    /// Estimated USD saved by routing cheap tasks locally
    /// (Σ counterfactual − Σ spend).
    pub savings_usd: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::route::{RouteDecision, TaskType};
    use tempfile::TempDir;

    fn make_event(escalate: bool) -> EscalationEvent {
        EscalationEvent {
            timestamp: "2026-06-01T12:00:00Z".into(),
            task_summary: "test task".into(),
            difficulty_score: if escalate { 0.82 } else { 0.15 },
            task_type: TaskType::General,
            estimated_context_tokens: 1000,
            decision: if escalate {
                RouteDecision::Escalate {
                    model_id: "anthropic_opus".into(),
                    reason: "high difficulty".into(),
                }
            } else {
                RouteDecision::Local {
                    model_id: "ollama_llama3".into(),
                    reason: "low difficulty".into(),
                }
            },
            role: None,
            escalation_from: if escalate {
                Some("ollama_llama3".into())
            } else {
                None
            },
            estimated_cost_usd: if escalate { 0.015 } else { 0.0 },
            counterfactual_cost_usd: 0.015,
        }
    }

    #[test]
    fn append_and_replay_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
        ledger.append(&make_event(true)).unwrap();
        ledger.append(&make_event(false)).unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let events = EscalationLedger::replay_today(tmp.path());
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn open_creates_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub").join("deep");
        EscalationLedger::open(&sub).unwrap();
        assert!(sub.exists());
    }

    #[test]
    fn escalation_rate_computes_correctly() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
        // 3 local, 2 escalate → rate = 2/5 = 0.4
        ledger.append(&make_event(false)).unwrap(); // local
        ledger.append(&make_event(true)).unwrap();  // escalate
        ledger.append(&make_event(false)).unwrap(); // local
        ledger.append(&make_event(false)).unwrap(); // local
        ledger.append(&make_event(true)).unwrap();  // escalate
        ledger.flush().unwrap();
        drop(ledger);

        let (esc, total, rate) = EscalationLedger::escalation_rate(tmp.path(), 1);
        assert_eq!(esc, 2);
        assert_eq!(total, 5);
        assert!((rate - 0.4).abs() < 0.001, "rate={rate}, expected 0.4");
    }

    #[test]
    fn empty_ledger_rate_is_zero() {
        let tmp = TempDir::new().unwrap();
        let (_esc, total, rate) = EscalationLedger::escalation_rate(tmp.path(), 7);
        assert_eq!(total, 0);
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn summary_reports_spend_and_savings() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
        // 3 local (each avoids $0.015), 2 escalate (each spends $0.015).
        for escalate in [false, true, false, false, true] {
            ledger.append(&make_event(escalate)).unwrap();
        }
        ledger.flush().unwrap();
        drop(ledger);

        let s = EscalationLedger::summary(tmp.path(), 1);
        assert_eq!(s.escalations, 2);
        assert_eq!(s.total, 5);
        assert!((s.spend_usd - 0.030).abs() < 1e-9, "spend={}", s.spend_usd);
        assert!((s.savings_usd - 0.045).abs() < 1e-9, "savings={}", s.savings_usd);
    }
}
```

Add the module to `mur-core/src/route/mod.rs` after the `pub mod heuristic;` line:

```rust
pub mod ledger;
```

- [ ] **Step 4: Expand integration tests and verify**

Replace `mur-core/tests/route_ledger.rs` (reuses the shared `make_event`, adds a
cost-savings assertion):

```rust
mod common;
use common::make_event;
use mur_common::route::RouteDecision;
use mur_core::route::ledger::EscalationLedger;
use tempfile::TempDir;

#[test]
fn ledger_appends_and_replays() {
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
    ledger.append(&make_event(true)).unwrap();
    ledger.append(&make_event(false)).unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let events = EscalationLedger::replay_today(tmp.path());
    assert_eq!(events.len(), 2);
    let escalations: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.decision, RouteDecision::Escalate { .. }))
        .collect();
    assert_eq!(escalations.len(), 1);
}

#[test]
fn summary_reports_rate_and_savings() {
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
    // 3 local (each avoids $0.015), 2 escalate (each spends $0.015).
    for escalate in [false, true, false, false, true] {
        ledger.append(&make_event(escalate)).unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);

    let s = EscalationLedger::summary(tmp.path(), 1);
    assert_eq!(s.escalations, 2);
    assert_eq!(s.total, 5);
    assert!((s.rate - 0.4).abs() < 0.001);
    assert!((s.spend_usd - 0.030).abs() < 1e-9, "spend={}", s.spend_usd);
    assert!((s.savings_usd - 0.045).abs() < 1e-9, "savings={}", s.savings_usd);
}

#[test]
fn empty_ledger_has_zero_summary() {
    let tmp = TempDir::new().unwrap();
    let s = EscalationLedger::summary(tmp.path(), 7);
    assert_eq!(s.total, 0);
    assert_eq!(s.rate, 0.0);
    assert_eq!(s.savings_usd, 0.0);
}
```

Run tests:
```
cargo test -p mur-core route::ledger::tests
cargo test -p mur-core route_ledger
```
Expected: 5 unit tests PASS, 3 integration tests PASS

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/route/ledger.rs mur-core/src/route/mod.rs mur-core/tests/route_ledger.rs
git commit -m "feat(route): add EscalationLedger for audit trail

Wraps GenericLedger<EscalationEvent> and adds summary() — escalation
rate plus estimated spend/savings in USD (the savings surface).
escalation_rate() now delegates to summary(). Stored at
~/.mur/route/ledger/YYYY-MM-DD.jsonl via the canonical paths::mur_root.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: CLI — `mur model route estimate`

**Files:**
- Modify: `mur-core/src/cmd/model.rs`

- [ ] **Step 1: Add Route subcommand to ModelCmd enum**

Add to the `ModelCmd` enum in `mur-core/src/cmd/model.rs` (alongside the existing `Role` variant):

```rust
    /// Test routing decisions (dry-run — no spawn).
    Route {
        #[command(subcommand)]
        sub: RouteSubCmd,
    },
```

Add the `RouteSubCmd` enum after the `RoleSubCmd` enum:

```rust
#[derive(Subcommand, Debug)]
pub enum RouteSubCmd {
    /// Estimate difficulty and routing decision for a task (dry-run).
    Estimate {
        /// Task description.
        prompt: String,
        /// Task type for difficulty scoring.
        #[arg(long, default_value = "general")]
        task_type: String,
        /// Estimated context tokens needed.
        #[arg(long, default_value_t = 1000)]
        tokens: u64,
        /// Role for override lookup.
        #[arg(long)]
        role: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Persist this decision to the escalation ledger.
        #[arg(long)]
        record: bool,
    },
    /// Show escalation audit ledger statistics.
    Ledger {
        /// Number of days to scan.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}
```

- [ ] **Step 2: Add dispatch arms in the run() function**

In `cmd/model.rs`'s `run()` function, add alongside the existing `ModelCmd::Role { sub }` match arm:

```rust
        ModelCmd::Route { sub } => cmd_route(sub, &reg)?,
```

- [ ] **Step 3: Implement cmd_route function**

Add at the bottom of `cmd/model.rs` (before the closing `}` of any existing function):

```rust
fn cmd_route(sub: &RouteSubCmd, reg: &ModelRegistry) -> anyhow::Result<()> {
    match sub {
        RouteSubCmd::Estimate {
            prompt,
            task_type,
            tokens,
            role,
            json,
            record,
        } => {
            let tt = parse_task_type(task_type)?;
            let router = Router::new(reg.clone())?;
            let timestamp = chrono::Utc::now().to_rfc3339();
            let event = router.audit(prompt, tt, *tokens, role.as_deref(), &timestamp);

            // Optionally persist so `ledger` can show the savings.
            if *record {
                let mut ledger = EscalationLedger::open_default()?;
                ledger.append(&event)?;
                ledger.flush()?;
            }

            if *json {
                let out = serde_json::json!({
                    "prompt": prompt,
                    "task_type": task_type,
                    "estimated_tokens": tokens,
                    "role": role,
                    "difficulty_score": event.difficulty_score,
                    "decision": event.decision,
                    "estimated_cost_usd": event.estimated_cost_usd,
                    "counterfactual_cost_usd": event.counterfactual_cost_usd,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Task:             {prompt}");
                println!("Type:             {task_type}");
                println!("Tokens:           ~{tokens}");
                if let Some(r) = role {
                    println!("Role:             {r}");
                }
                println!("Score:            {:.3}", event.difficulty_score);
                println!("Decision:         {}", event.decision);
                println!("Est. cost:        ${:.4}", event.estimated_cost_usd);
                if event.counterfactual_cost_usd > 0.0 {
                    println!(
                        "Savings (if local): ${:.4}",
                        event.counterfactual_cost_usd - event.estimated_cost_usd
                    );
                }
            }
        }
        RouteSubCmd::Ledger { days, json } => {
            let ledger_dir = crate::paths::mur_root(None).join("route").join("ledger");
            let events = EscalationLedger::replay_days(&ledger_dir, *days);
            let s = EscalationLedger::summary(&ledger_dir, *days);

            if *json {
                let out = serde_json::json!({
                    "days_scanned": days,
                    "total_decisions": s.total,
                    "escalations": s.escalations,
                    "escalation_rate": s.rate,
                    "spend_usd": s.spend_usd,
                    "savings_usd": s.savings_usd,
                    "events": events,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                println!("Escalation ledger ({days}d):");
                println!("  Total decisions: {}", s.total);
                println!("  Escalations:     {}", s.escalations);
                println!("  Escalation rate: {:.1}%", s.rate * 100.0);
                println!("  Spend:           ${:.4}", s.spend_usd);
                println!("  Savings:         ${:.4}", s.savings_usd);
                if s.total > 0 {
                    println!();
                    for event in &events {
                        let mark = match &event.decision {
                            RouteDecision::Local { .. } => "LOCAL",
                            RouteDecision::Escalate { .. } => "ESCALATE",
                        };
                        println!(
                            "  [{mark}] {:.3} | ${:.4} | {}",
                            event.difficulty_score, event.estimated_cost_usd, event.task_summary,
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_task_type(s: &str) -> anyhow::Result<TaskType> {
    match s.to_lowercase().as_str() {
        "codegen" | "code_gen" | "code" => Ok(TaskType::CodeGen),
        "codereview" | "code_review" | "review" => Ok(TaskType::CodeReview),
        "retrieval" | "search" | "retrieve" => Ok(TaskType::Retrieval),
        "refactor" => Ok(TaskType::Refactor),
        "documentation" | "docs" | "doc" => Ok(TaskType::Documentation),
        "debugging" | "debug" => Ok(TaskType::Debugging),
        "execution" | "exec" | "run" => Ok(TaskType::Execution),
        "general" | "chat" | "qa" => Ok(TaskType::General),
        other => anyhow::bail!(
            "unknown task type: {other}. Valid types: codegen, codereview, retrieval, \
             refactor, documentation, debugging, execution, general"
        ),
    }
}
```

Add imports at the top of `cmd/model.rs` (after the existing imports):

```rust
use chrono::Utc;
use mur_common::route::{RouteDecision, TaskType};
use crate::route::ledger::EscalationLedger;
use crate::route::Router;
```

- [ ] **Step 4: Build and smoke-test**

Build: `cargo build --release`

Manual smoke test (no ledger yet — just verify the estimate path works):
```bash
# Should route to local (shows cost/savings)
cargo run -- model route estimate "run cargo fmt" --task-type execution --tokens 200

# Should route to frontier
cargo run -- model route estimate "refactor auth system across modules" --task-type refactor --tokens 8000

# JSON output (includes cost fields)
cargo run -- model route estimate "fix typo" --task-type documentation --tokens 100 --json

# Record a few decisions to populate the ledger
cargo run -- model route estimate "run test" --task-type execution --tokens 100 --record
cargo run -- model route estimate "refactor auth" --task-type refactor --tokens 8000 --record

# Ledger with cost savings
cargo run -- model route ledger
```

Expected: Estimate output includes `Est. cost` and (when local) `Savings` lines.
Ledger shows escalation rate plus `Spend`/`Savings` in USD.

- [ ] **Step 5: Write integration-level CLI test**

Create `mur-core/tests/cmd_model_route.rs`:

```rust
use std::process::Command;

/// Helper: run `mur model route estimate` and check exit code.
fn estimate(prompt: &str, task_type: &str, tokens: u64) -> String {
    let output = Command::new(
        std::env::var("CARGO_BIN_EXE_mur").unwrap_or_else(|_| "target/release/mur".into()),
    )
    .args([
        "model", "route", "estimate", prompt,
        "--task-type", task_type,
        "--tokens", &tokens.to_string(),
    ])
    .env("MUR_HOME", std::env::temp_dir().join("mur-test-route"))
    .output()
    .expect("failed to run mur");
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn estimate_outputs_score_and_decision() {
    let out = estimate("echo hello", "execution", 50);
    assert!(out.contains("Score:"), "should contain Score: line, got: {out}");
    assert!(out.contains("Decision:"), "should contain Decision: line, got: {out}");
}

#[test]
fn estimate_json_output_is_valid() {
    let output = Command::new(
        std::env::var("CARGO_BIN_EXE_mur").unwrap_or_else(|_| "target/release/mur".into()),
    )
    .args([
        "model", "route", "estimate", "refactor auth",
        "--task-type", "refactor",
        "--tokens", "5000",
        "--json",
    ])
    .env("MUR_HOME", std::env::temp_dir().join("mur-test-route-json"))
    .output()
    .expect("failed to run mur");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .expect("output should be valid JSON");
    assert!(v["difficulty_score"].as_f64().is_some());
    assert!(v["decision"].as_object().is_some());
}
```

Run: `cargo test -p mur-core cmd_model_route`
Expected: 2 tests PASS

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/model.rs mur-core/tests/cmd_model_route.rs
git commit -m "feat(route): add 'mur model route estimate' and 'ledger' CLI

mur model route estimate <prompt> --task-type <type> --tokens <N>
  Dry-run: prints difficulty score and routing decision without spawning.

mur model route ledger [--days N] [--json]
  Shows escalation count, rate, and event log from the audit ledger.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: CLI — extend `mur model role set` with routing overrides

**Files:**
- Modify: `mur-core/src/cmd/model.rs`

- [ ] **Step 1: Add --route-policy flag to RoleSubCmd::Set**

In `cmd/model.rs`, modify the `RoleSubCmd::Set` variant to add the new flag:

```rust
    Set {
        role: String,
        model: String,
        #[arg(long)]
        fallback: Option<String>,
        #[arg(long)]
        budget: Option<f64>,
        #[arg(long)]
        privacy_local_only: bool,
        /// Routing policy: auto, prefer-local, force-local, or
        /// force-frontier:<model-id>.
        #[arg(long)]
        route_policy: Option<String>,
    },
```

- [ ] **Step 2: Parse route_policy and store in RoleEntry**

In `cmd/model.rs`, modify the `cmd_role()` function. Replace the `RoleSubCmd::Set { ... }` match arm:

```rust
        RoleSubCmd::Set {
            role,
            model,
            fallback,
            budget,
            privacy_local_only,
            route_policy,
        } => {
            let policy = route_policy
                .as_deref()
                .map(parse_route_policy)
                .transpose()?;
            reg.roles.insert(
                role.clone(),
                RoleEntry {
                    primary: model.clone(),
                    fallback,
                    cost_budget_per_day_usd: budget,
                    privacy_local_only,
                    route_policy: policy,
                },
            );
            reg.save_to(path)?;
            println!("Role {role} → {model}");
            if let Some(ref p) = route_policy {
                println!("  route policy: {p}");
            }
        }
```

Add the `parse_route_policy` helper function:

```rust
fn parse_route_policy(raw: &str) -> anyhow::Result<RoutePolicy> {
    match raw {
        "auto" => Ok(RoutePolicy::Auto),
        "prefer-local" | "prefer_local" => Ok(RoutePolicy::PreferLocal),
        "force-local" | "force_local" => Ok(RoutePolicy::ForceLocal),
        other if other.starts_with("force-frontier:") || other.starts_with("force_frontier:") => {
            let model_id = other
                .splitn(2, ':')
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("force-frontier requires :<model-id>"))?
                .trim()
                .to_string();
            if model_id.is_empty() {
                anyhow::bail!("force-frontier requires a model ID after ':'");
            }
            Ok(RoutePolicy::ForceFrontier { model_id })
        }
        other => anyhow::bail!(
            "invalid route policy: {other}. Valid: auto, prefer-local, force-local, \
             force-frontier:<model-id>"
        ),
    }
}
```

Add the import for `RoutePolicy` (should already be present from Task 6's imports, but verify):

```rust
use mur_common::route::{RouteDecision, RoutePolicy, TaskType};
```

- [ ] **Step 3: Build and smoke-test**

Build: `cargo build --release`

Manual test:
```bash
# Set up test models (use temp MUR_HOME)
export MUR_HOME=$(mktemp -d)
cargo run -- model add ollama_local --provider ollama --model llama3.2:3b --tier local
cargo run -- model add anthropic_opus --provider anthropic --model claude-opus-4-7 --tier frontier

# Set a role with force-local
cargo run -- model role set reflector ollama_local --route-policy force-local

# Set a role with force-frontier
cargo run -- model role set dev anthropic_opus --route-policy force-frontier:anthropic_opus

# Set a role with prefer-local
cargo run -- model role set chat ollama_local --route-policy prefer-local

# Verify
cargo run -- model role list

# Test that the routing picks up the role overrides
cargo run -- model route estimate "refactor auth" --task-type refactor --tokens 8000 --role dev
cargo run -- model route estimate "echo hello" --task-type execution --tokens 50 --role reflector

# Cleanup
rm -rf "$MUR_HOME"
```

Expected: role list shows the policies; estimate with role `dev` (force_frontier) always escalates; estimate with role `reflector` (force_local) always stays local.

- [ ] **Step 4: Wire --tier flag into ModelCmd::Add**

While we're here, the `ModelCmd::Add` path should accept `--tier` and `--cost-per-1k` flags. Add to the `Add` variant:

```rust
    Add {
        name: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        secret: Option<String>,
        #[arg(long, value_delimiter = ',')]
        capabilities: Vec<String>,
        /// Routing tier: local or frontier.
        #[arg(long)]
        tier: Option<String>,
        /// Estimated USD per 1000 output tokens.
        #[arg(long)]
        cost_per_1k: Option<f64>,
    },
```

And in the `Add` handler, update the `ModelEntry` construction (the `tier: None, cost_per_1k_tokens: None,` stopgaps from Task 2 Step 4):

```rust
            let tier = tier
                .as_deref()
                .map(|t| match t.to_lowercase().as_str() {
                    "local" => Ok(RouteTier::Local),
                    "frontier" => Ok(RouteTier::Frontier),
                    other => anyhow::bail!(
                        "invalid tier: {other}. Valid: local, frontier"
                    ),
                })
                .transpose()?;
            reg.models.insert(
                name.clone(),
                ModelEntry {
                    provider,
                    model,
                    base_url,
                    secret: secret_ref,
                    capabilities,
                    params: serde_json::Value::Null,
                    tier,
                    cost_per_1k_tokens: cost_per_1k,
                },
            );
```

Add the `RouteTier` import (should already be present):

```rust
use mur_common::route::{RouteDecision, RoutePolicy, RouteTier, TaskType};
```

- [ ] **Step 5: Test end-to-end**

```bash
export MUR_HOME=$(mktemp -d)
cargo run -- model add ollama_local --provider ollama --model llama3 --tier local
cargo run -- model add claude_sonnet --provider anthropic --model claude-sonnet-4-6 --tier frontier --cost-per-1k 0.003
cargo run -- model list

# Verify tiers show up
cargo run -- model show ollama_local | grep -q "tier: local" && echo "tier OK"
cargo run -- model show claude_sonnet | grep -q "tier: frontier" && echo "tier OK"

# Cleanup
rm -rf "$MUR_HOME"
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/model.rs
git commit -m "feat(route): add --route-policy to 'mur model role set' and --tier to 'add'

mur model role set <role> <model> --route-policy {auto,prefer-local,
force-local,force-frontier:<id>}

mur model add <name> --tier {local,frontier} --cost-per-1k <usd>

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 8: Full pipeline integration test

**Files:**
- Create: `mur-core/tests/route_pipeline.rs`

- [ ] **Step 1: Write the end-to-end integration test**

```rust
//! End-to-end test: model registry → router → escalation ledger.
//!
//! Simulates the complete Phase 1 flow: register tiered models, configure
//! a role with routing overrides, route several tasks, and verify the
//! escalation ledger captures decisions + cost savings correctly.

mod common;
use common::test_registry;
use mur_common::model::RoleEntry;
use mur_common::route::{RouteDecision, RoutePolicy, TaskType};
use mur_core::route::ledger::EscalationLedger;
use mur_core::route::Router;
use tempfile::TempDir;

fn setup_registry_with_roles() -> mur_common::model::ModelRegistry {
    let mut reg = test_registry();
    reg.roles.insert(
        "dev".into(),
        RoleEntry {
            primary: "anthropic_opus".into(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: Some(RoutePolicy::ForceFrontier {
                model_id: "anthropic_opus".into(),
            }),
        },
    );
    reg.roles.insert(
        "reflector".into(),
        RoleEntry {
            primary: "ollama_llama3".into(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: Some(RoutePolicy::ForceLocal),
        },
    );
    reg
}

#[test]
fn full_pipeline_routes_records_and_tracks_cost() {
    let reg = setup_registry_with_roles();
    let router = Router::new(reg).unwrap();
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();

    let tasks = vec![
        ("Run unit tests", TaskType::Execution, 200_u64, Some("dev")),
        ("Summarize chat history", TaskType::General, 1500, Some("reflector")),
        ("Add a docstring", TaskType::Documentation, 300, None),
        ("Refactor auth module", TaskType::Refactor, 8000, None),
        ("Fix typo in README", TaskType::Documentation, 100, Some("reflector")),
    ];

    let mut local_count = 0;
    let mut escalate_count = 0;

    for (summary, task_type, tokens, role) in &tasks {
        // Use audit() for proper cost-tracking from the start.
        let event = router.audit(summary, task_type.clone(), *tokens, *role, "2026-06-01T12:00:00Z");
        match &event.decision {
            RouteDecision::Local { .. } => local_count += 1,
            RouteDecision::Escalate { .. } => escalate_count += 1,
        }
        // cost fields must be populated
        if let RouteDecision::Escalate { .. } = &event.decision {
            assert!(event.estimated_cost_usd > 0.0, "escalated task must have a cost");
        }
        assert!(event.counterfactual_cost_usd > 0.0, "all tasks have a counterfactual cost");
        ledger.append(&event).unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);

    // Routing: dev→escalate, reflector→local, no-role→depends-on-difficulty.
    assert_eq!(local_count, 3, "reflector tasks + easy doc task should be local");
    assert_eq!(escalate_count, 2, "dev task + hard refactor should escalate");

    // Ledger + cost summary.
    let events = EscalationLedger::replay_today(tmp.path());
    assert_eq!(events.len(), 5);

    let s = EscalationLedger::summary(tmp.path(), 1);
    assert_eq!(s.total, 5);
    assert_eq!(s.escalations, 2);
    assert!((s.rate - 0.4).abs() < 0.001, "rate={}", s.rate);
    // 2 escalations × 200+8000=8200 tokens × $0.015/1k ≈ $0.123
    assert!(s.spend_usd > 0.0, "spend should be > 0");
    // 3 local tasks avoided frontier cost → savings > 0
    assert!(s.savings_usd > 0.0, "savings should be > 0");
    assert!(
        s.savings_usd > s.spend_usd,
        "more money saved than spent: savings={} spend={}",
        s.savings_usd,
        s.spend_usd,
    );

    // Verify specific decisions.
    let dev_event = events.iter().find(|e| e.role.as_deref() == Some("dev")).unwrap();
    assert!(matches!(dev_event.decision, RouteDecision::Escalate { .. }));
    assert_eq!(dev_event.task_type, TaskType::Execution);

    let reflector_event = events
        .iter()
        .find(|e| e.role.as_deref() == Some("reflector"))
        .unwrap();
    assert!(matches!(reflector_event.decision, RouteDecision::Local { .. }));
}

#[test]
fn empty_registry_is_rejected() {
    let reg = mur_common::model::ModelRegistry::default();
    let err = Router::new(reg).unwrap_err();
    assert!(
        err.to_string().contains("empty"),
        "empty registry should error: {err}"
    );
}

#[test]
fn escalation_rate_decreases_with_more_local_tasks() {
    let reg = setup_registry_with_roles();
    let router = Router::new(reg).unwrap();
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();

    let easy_tasks = [
        ("run cargo fmt", TaskType::Execution, 100_u64),
        ("echo hello", TaskType::Execution, 50),
        ("list files", TaskType::Execution, 75),
        ("check git status", TaskType::Execution, 80),
        ("print working dir", TaskType::Execution, 60),
    ];

    let hard_tasks = [
        ("refactor auth", TaskType::Refactor, 9000_u64),
        ("rewrite database layer", TaskType::CodeGen, 12000),
        ("fix race condition in scheduler", TaskType::Debugging, 7000),
    ];

    for (summary, tt, tokens) in &hard_tasks {
        let event = router.audit(summary, tt.clone(), *tokens, None, "2026-06-01T12:00:00Z");
        ledger.append(&event).unwrap();
    }
    for (summary, tt, tokens) in &easy_tasks {
        let event = router.audit(summary, tt.clone(), *tokens, None, "2026-06-01T12:01:00Z");
        ledger.append(&event).unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);

    let s = EscalationLedger::summary(tmp.path(), 1);
    assert_eq!(s.total, 8);
    // Hard tasks (3) + easy tasks (5) → 3 escalations → rate 3/8 = 0.375
    assert!(s.rate > 0.3 && s.rate < 0.45, "rate={}, expected ~0.375", s.rate);
    assert!(s.savings_usd > s.spend_usd, "more savings than spend");
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p mur-core route_pipeline`
Expected: 3 tests PASS (empty_registry_is_rejected, full_pipeline, escalation_rate)

- [ ] **Step 3: Run the full test suite to check for regressions**

Run: `cargo test -p mur-common`
Expected: All existing tests PASS (especially the model registry round-trip tests)

Run: `cargo test -p mur-core`
Expected: All tests PASS (including new route tests and existing tests)

Run: `cargo clippy -p mur-common -p mur-core -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Commit**

```bash
git add mur-core/tests/route_pipeline.rs
git commit -m "test(route): add end-to-end pipeline integration test

Covers the full Phase 1 flow: registry → router → ledger. Verifies
force_frontier/force_local overrides, difficulty-based routing, and
escalation rate computation across mixed task profiles.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review

### 1. Spec Coverage

| Spec requirement | Task |
|---|---|
| Router on the model registry | Task 4 — `Router` wraps `ModelRegistry`; constructor errors on empty reg |
| Hybrid routing (auto + override) | Task 4 — `decide_with_score()` checks per-role policy first, then falls through to heuristic; single source of truth |
| Difficulty heuristic with named weights | Task 3 — `DefaultHeuristic` with `WEIGHT_BASE`/`WEIGHT_CONTEXT`/`WEIGHT_KEYWORD` constants + normalized `keyword_boost` |
| `PreferLocal` threshold is reachable | Task 3 — `KEYWORD_SATURATION=3` → normalized boost [0,1]; realistic max ≈ 0.85 > 0.75; locked by `prefer_local_still_escalates_extreme_tasks` test |
| Per-role override | Task 2 — `RoleEntry.route_policy`; Task 7 — CLI `--route-policy` flag |
| Audit ledger with cost-savings metric | Task 5 — `EscalationLedger::summary()` returns `LedgerSummary` (escalations, rate, spend_usd, savings_usd) |
| Cost savings measurable (the goal) | Tasks 4+6+8 — `Router::audit()` populates `estimated_cost_usd` + `counterfactual_cost_usd`; CLI `estimate` prints both; `ledger` prints `Spend`/`Savings`; Task 8 asserts `savings > spend` |
| Audit ledger path `~/.mur/route/ledger/` | Task 5 — `EscalationLedger::open_default()` via `crate::paths::mur_root(None)` (no private reimplementation) |
| `RouteTier` annotation on models + tier inference | Task 2 — `ModelEntry.tier: Option<RouteTier>`; Task 4 — `effective_tier()` infers from provider when absent |
| `RoutePolicy` variants (Auto, PreferLocal, ForceLocal, ForceFrontier) | Task 1 — `RoutePolicy` enum with all four variants |
| Cost-per-token estimate | Task 2 — `ModelEntry.cost_per_1k_tokens`; Task 7 — `--cost-per-1k` flag; Task 4 — `frontier_cost_per_1k()` for audit |
| CLI to test routing decisions | Task 6 — `mur model route estimate` (dry-run, --record persists) |
| CLI to view ledger with savings | Task 6 — `mur model route ledger` shows Spend/Savings in USD |
| No `"unknown"` sentinel model IDs | Task 4 — empty registry rejected at `Router::new`; cross-tier degradation uses `expect` with invariant message |
| No hardcoded values | `WEIGHT_BASE`/`WEIGHT_CONTEXT`/`WEIGHT_KEYWORD`/`KEYWORD_SATURATION` in heuristic; `DEFAULT_ESCALATION_THRESHOLD`/`PREFER_LOCAL_THRESHOLD` in router; `LOCAL_PROVIDERS` for inference |
| Follows existing store conventions | Task 5 — atomic write via existing `Ledger<E>` (no new store needed) |
| Shared test fixtures (DRY) | Task 4 — `tests/common/mod.rs` with `test_registry()` + `make_event()`; reused by Tasks 5 & 8 |
| Tests red before green (no `todo!()` theater) | Tasks 1/3/4/5 Step 1 — real `Router`/`EscalationLedger`-imported tests that fail to **compile** before the module exists |
| Phase 2 spawn NOT implemented | Confirmed — nothing in this plan touches subprocess spawning |

### 2. Placeholder Scan

- No "TBD", "TODO", "implement later" in any code block.
- No `todo!("compile guard")` — all Step 1 tests are genuine compile-reds.
- No "add appropriate error handling" — every error path has explicit `anyhow::bail!` or `Result` propagation.
- No "write tests for the above" — every task includes the actual test code.
- No line-number edit anchors — all edits reference nearby symbols (e.g. "alongside `pub mod retrieve;`", "in the `RoleSubCmd::Set` match arm").

### 3. Type Consistency

- `RouteTier` defined in Task 1, used in Task 2 (`ModelEntry.tier`), Task 4 (`effective_tier`/`pick_best`), Task 7 (`--tier` flag).
- `RouteDecision` defined in Task 1, used in Task 4 (`Router`), Task 5 (`EscalationEvent.decision`), Task 6 (CLI output).
- `RoutePolicy` defined in Task 1, used in Task 2 (`RoleEntry.route_policy`), Task 4 (`role_policy()`), Task 7 (`--route-policy` flag).
- `EscalationEvent` defined in Task 1, used in Task 4 (`Router::audit`), Task 5 (`EscalationLedger`), Tasks 6 & 8.
- `TaskType` defined in Task 1, used in Task 3 (`Heuristic::score`), Task 4 (`Router`), Task 6 (`parse_task_type`), Task 8.
- `LedgerSummary` defined in Task 5, consumed by CLI (Task 6) and integration tests (Task 8).
- `EscalationLedger` created in Task 5; `open_default` reuses `crate::paths::mur_root(None)` (no private copy).
- **Single routing path:** `Router::decide()` delegates to `decide_with_score()`; locked by `decide_matches_decide_with_score_for_auto_role`.
- **Workspace compiles at every commit:** Task 2 Step 4 updates all 6 existing `ModelEntry` literals + 1 full `RoleEntry` literal in `mur-core` with `None` stopgaps.
- **`Router::new` returns `Result`:** empty registry is an error, not a silent `"unknown"` sentinel; every call site uses `.unwrap()` (tests) or `?` (CLI).

---

**Plan complete. 8 tasks.** Estimated time: 4-6 hours for a solo developer; 45-90 minutes with subagent-driven parallel execution.
