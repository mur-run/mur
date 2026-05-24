# MUR Coordination Protocol M0 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the `mur_common::coordination` module — Plan TOML schema, shared type enums, validation, content hash, conformance adapter trait, and 10 plan-loading conformance tests.

**Architecture:** New module `mur-common/src/coordination/` with three sub-modules (`types`, `plan`, `conformance`) plus one integration test file. Zero breaking changes — all types are additive in a new namespace. Commander gets these types automatically via its existing `mur-common` git dependency when it bumps the tag.

**Tech Stack:** Rust, serde, toml, sha2, uuid, thiserror (all already in mur-common deps or workspace deps).

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `mur-common/Cargo.toml` | **Modify** | Add `toml` dependency |
| `mur-common/src/lib.rs` | **Modify** | Add `pub mod coordination;` |
| `mur-common/src/coordination/mod.rs` | **Create** | Module root — re-exports, module declarations |
| `mur-common/src/coordination/types.rs` | **Create** | `Phase`, `DeterminismMode`, `FailureCategory`, `RecoveryAction`, `ConformanceLevel` |
| `mur-common/src/coordination/plan.rs` | **Create** | `Plan`, `Step`, TOML deser, validation, `content_sha256`, `canonical_bytes` |
| `mur-common/src/coordination/conformance.rs` | **Create** | `ConformanceAdapter` trait, `PlanLoadingSuite`, test runner |
| `mur-common/tests/coordination_conformance.rs` | **Create** | 10 integration tests for plan loading + validation |

---

### Task 1: Add `toml` dependency to mur-common

**Files:**
- Modify: `mur-common/Cargo.toml`

- [ ] **Step 1: Add `toml` to dependencies**

Open `mur-common/Cargo.toml`. Add `toml` after the existing `sha2` line:

```toml
sha2 = "0.10"
toml = "0.8"
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo check -p mur-common 2>&1 | tail -5`
Expected: Compiles without errors (dependency resolves, no code uses it yet).

- [ ] **Step 3: Commit**

```bash
git add mur-common/Cargo.toml
git commit -m "chore(mur-common): add toml dependency for coordination plan schema

M0 prep — toml 0.8 for Plan/Step TOML deserialization.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Coordination type enums

**Files:**
- Create: `mur-common/src/coordination/mod.rs`
- Create: `mur-common/src/coordination/types.rs`
- Modify: `mur-common/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `mur-common/src/coordination/types.rs` with test module only (no types yet):

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_phase_deser() {
        // Will fail — Phase enum doesn't exist yet
        let phase: super::Phase = serde_json::from_str(r#""implement""#).unwrap();
        assert_eq!(phase, super::Phase::Implement);
    }

    #[test]
    fn test_phase_ordering() {
        // Verify the SDLC phases are in correct order
        use super::Phase;
        let ordered = vec![
            Phase::Plan,
            Phase::Design,
            Phase::Implement,
            Phase::Test,
            Phase::Verify,
        ];
        for (i, phase) in ordered.iter().enumerate() {
            assert_eq!(phase.sdlc_index(), i as u8);
        }
    }

    #[test]
    fn test_determinism_mode_deser() {
        let strict: super::DeterminismMode = serde_json::from_str(r#""strict""#).unwrap();
        assert_eq!(strict, super::DeterminismMode::Strict);
        let be: super::DeterminismMode = serde_json::from_str(r#""best-effort""#).unwrap();
        assert_eq!(be, super::DeterminismMode::BestEffort);
    }

    #[test]
    fn test_determinism_mode_default() {
        assert_eq!(
            super::DeterminismMode::default(),
            super::DeterminismMode::BestEffort
        );
    }

    #[test]
    fn test_failure_category_deser() {
        let cat: super::FailureCategory = serde_json::from_str(r#""knowledge""#).unwrap();
        assert_eq!(cat, super::FailureCategory::Knowledge);
        let cat: super::FailureCategory = serde_json::from_str(r#""tool""#).unwrap();
        assert_eq!(cat, super::FailureCategory::Tool);
        let cat: super::FailureCategory = serde_json::from_str(r#""verify_failed""#).unwrap();
        assert_eq!(cat, super::FailureCategory::VerifyFailed);
    }

    #[test]
    fn test_recovery_action_serde_roundtrip() {
        let action = super::RecoveryAction::Reroute {
            reason: super::FailureCategory::Knowledge,
        };
        let json = serde_json::to_string(&action).unwrap();
        let roundtripped: super::RecoveryAction = serde_json::from_str(&json).unwrap();
        match roundtripped {
            super::RecoveryAction::Reroute { reason } => {
                assert_eq!(reason, super::FailureCategory::Knowledge);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_retry_variant_no_reason() {
        let action = super::RecoveryAction::Retry;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, r#"{"kind":"retry"}"#);
        let parsed: super::RecoveryAction = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, super::RecoveryAction::Retry));
    }

    #[test]
    fn test_conformance_level_ordering() {
        use super::ConformanceLevel;
        assert!(ConformanceLevel::Standard > ConformanceLevel::Minimal);
        assert!(ConformanceLevel::Full > ConformanceLevel::Standard);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-common --lib coordination::types::tests 2>&1 | tail -10`
Expected: Compilation errors — no `coordination` module yet.

- [ ] **Step 3: Create `mur-common/src/coordination/mod.rs`**

```rust
//! Coordination protocol types for multi-step agent workflows.
//!
//! This module defines the shared vocabulary for Plans, Steps, Microsteps,
//! SDLC phases, determinism modes, failure categories, and recovery actions.
//! It is the **single source of truth** for both mur-commander and mur-runtime.
//!
//! # Conformance
//!
//! Hosts implement [`ConformanceAdapter`] and pass [`PlanLoadingSuite`] to
//! prove they parse and validate plans correctly. See the
//! `tests/coordination_conformance.rs` integration test for the 10-test suite.

pub mod types;
pub mod plan;
pub mod conformance;

// Re-export the most commonly used types at the module root.
pub use types::{
    ConformanceLevel, DeterminismMode, FailureCategory, Phase, RecoveryAction,
};
pub use plan::{Plan, Step};
pub use conformance::{ConformanceAdapter, PlanLoadingSuite};
```

- [ ] **Step 4: Write `mur-common/src/coordination/types.rs` with the full type definitions**

```rust
//! Shared coordination types (§3, §7, §8 of the coordination protocol spec).
//!
//! These are pure data types with serde support. No I/O, no validation
//! logic — that lives in [`super::plan`].

use serde::{Deserialize, Serialize};

/// SDLC phase taxonomy (§3.2).
///
/// Each microstep declares a phase. The `verify` phase is special:
/// the agent cannot self-declare success on it — the host runs the
/// Verify Gateway as a subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Gather requirements, decompose the task.
    Plan,
    /// Choose approach, sketch types/contract.
    Design,
    /// Write code / produce artifacts.
    Implement,
    /// Run tests / validate.
    Test,
    /// Verify Gateway check (host-run, not agent-run).
    Verify,
}

impl Phase {
    /// Position in the SDLC order (0 = Plan, 4 = Verify).
    /// Used to validate that phases within a step are declared in order.
    pub fn sdlc_index(self) -> u8 {
        match self {
            Phase::Plan => 0,
            Phase::Design => 1,
            Phase::Implement => 2,
            Phase::Test => 3,
            Phase::Verify => 4,
        }
    }
}

/// Determinism mode for a plan or step (§7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeterminismMode {
    /// Fail the step immediately on budget/turn cap violation.
    Strict,
    /// Continue past caps with a warning trace.
    #[default]
    BestEffort,
}

/// Failure category taxonomy (§8.1).
///
/// From Trace2Skill (arXiv 2603.25158), shared with mur skill spec §8.2
/// and commander P1 journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    /// Agent lacked domain information.
    Knowledge,
    /// Wrong tool or tool parameters.
    Tool,
    /// Instructions were ambiguous.
    Clarification,
    /// Output format mismatch (content was correct).
    Style,
    /// Transient infrastructure failure (network, rate limit, timeout).
    Transient,
    /// Verify Gateway command exited non-zero.
    VerifyFailed,
}

/// Recovery action for a failed microstep (§8.2).
///
/// Serialized as a tagged enum: `{"kind": "retry"}` or
/// `{"kind": "reroute", "reason": "tool"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Retry same agent, same step, same parameters.
    Retry,
    /// Re-route to a different capable agent.
    Reroute {
        reason: FailureCategory,
    },
    /// Bubble to planner LLM for full re-planning.
    Escalate {
        reason: FailureCategory,
    },
    /// Give up; emit workflow_failed.
    Abort,
}

/// Host conformance level (§2.3).
///
/// Ordered: Minimal < Standard < Full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceLevel {
    /// Plan schema + microstep journal emission.
    Minimal,
    /// Minimal + Verify Gateway + Determinism + Recovery.
    Standard,
    /// Standard + Replay + Idempotency enforcement.
    Full,
}
```

- [ ] **Step 5: Add `pub mod coordination;` to `mur-common/src/lib.rs`**

In `mur-common/src/lib.rs`, add after the existing `pub mod companion;` line (alphabetical order):

```rust
pub mod coordination;
```

- [ ] **Step 6: Run the type tests**

Run: `cargo test -p mur-common --lib coordination::types::tests 2>&1 | tail -15`
Expected: All 8 tests pass.

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/lib.rs mur-common/src/coordination/
git commit -m "feat(coordination): add shared type enums for coordination protocol

M0 — Phase, DeterminismMode, FailureCategory, RecoveryAction,
ConformanceLevel enums with serde support. Single source of truth
for both mur-commander (P2 plan-and-execute) and mur-runtime.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: Plan and Step TOML schema

**Files:**
- Create: `mur-common/src/coordination/plan.rs`
- Modify: `mur-common/src/coordination/mod.rs` — already declares `pub mod plan;`

- [ ] **Step 1: Write the failing test for Plan TOML deserialization**

Create `mur-common/src/coordination/plan.rs` with only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::types::*;

    #[test]
    fn test_parse_minimal_plan() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "Add Stripe webhook handler"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:commander-planner"
budget_estimate_usd = 2.50
determinism = "best-effort"
content_sha256 = "abc123"

[[plan.steps]]
step_id = "step_001"
description = "Implement webhook signature validator"
agent_hint = "code-review"
phases = ["plan", "design", "implement", "test", "verify"]
verify_command = "cargo test --lib webhook_validator"
depends_on = []

[[plan.steps]]
step_id = "step_002"
description = "Deploy to staging"
agent_hint = "generic"
phases = ["plan", "implement", "verify"]
verify_command = "curl -fsS https://staging.example.com/health"
depends_on = ["step_001"]
"#;
        let plan: Plan = toml::from_str(toml).expect("parse valid plan");
        assert_eq!(plan.plan.version, "0");
        assert_eq!(
            plan.plan.plan_id,
            "550e8400-e29b-41d4-a716-446655440000".parse().unwrap()
        );
        assert_eq!(plan.plan.goal, "Add Stripe webhook handler");
        assert_eq!(plan.plan.determinism, DeterminismMode::BestEffort);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].step_id, "step_001");
        assert_eq!(plan.steps[0].agent_hint, "code-review");
        assert_eq!(
            plan.steps[0].phases,
            vec![
                Phase::Plan,
                Phase::Design,
                Phase::Implement,
                Phase::Test,
                Phase::Verify,
            ]
        );
        assert!(plan.steps[0].depends_on.is_empty());
        assert_eq!(plan.steps[1].depends_on, vec!["step_001".to_string()]);
    }

    #[test]
    fn test_parse_defaults() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc"

[[plan.steps]]
step_id = "s1"
description = "test step"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = []
"#;
        let plan: Plan = toml::from_str(toml).expect("parse defaults");
        // Optional fields default to None
        assert!(plan.plan.signature.is_none());
        assert!(plan.steps[0].skill_ref.is_none());
        assert_eq!(plan.steps[0].budget_estimate_usd, None);
        assert_eq!(plan.steps[0].timeout_secs, None);
        // determinism defaults to BestEffort when absent
        assert_eq!(plan.plan.determinism, DeterminismMode::BestEffort);
    }

    #[test]
    fn test_reject_unknown_phase() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc"

[[plan.steps]]
step_id = "s1"
description = "bad phase"
agent_hint = "generic"
phases = ["unknown_phase"]
verify_command = "true"
depends_on = []
"#;
        let result = toml::from_str::<Plan>(toml);
        assert!(result.is_err(), "unknown phase must reject");
    }

    #[test]
    fn test_reject_missing_verify_command() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc"

[[plan.steps]]
step_id = "s1"
description = "no verify"
agent_hint = "generic"
phases = ["plan"]
verify_command = ""
depends_on = []
"#;
        let plan = toml::from_str::<Plan>(toml).expect("parse ok");
        // Validation happens separately from parsing
        let errors = plan.validate();
        assert!(!errors.is_empty(), "empty verify_command must fail validation");
    }

    #[test]
    fn test_reject_missing_phases() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc"

[[plan.steps]]
step_id = "s1"
description = "no phases"
agent_hint = "generic"
phases = []
verify_command = "true"
depends_on = []
"#;
        let plan = toml::from_str::<Plan>(toml).expect("parse ok");
        let errors = plan.validate();
        assert!(!errors.is_empty(), "empty phases must fail validation");
    }

    #[test]
    fn test_validate_acyclic_dag() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc"

[[plan.steps]]
step_id = "s1"
description = "step 1"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = ["s2"]

[[plan.steps]]
step_id = "s2"
description = "step 2"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = ["s1"]
"#;
        let plan = toml::from_str::<Plan>(toml).expect("parse ok");
        let errors = plan.validate();
        assert!(!errors.is_empty(), "cycle must fail validation");
        let has_cycle = errors.iter().any(|e| e.contains("cycle"));
        assert!(has_cycle, "error must mention cycle, got: {:?}", errors);
    }

    #[test]
    fn test_validate_unknown_dependency() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc"

[[plan.steps]]
step_id = "s1"
description = "step 1"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = ["nonexistent_step"]
"#;
        let plan = toml::from_str::<Plan>(toml).expect("parse ok");
        let errors = plan.validate();
        assert!(!errors.is_empty(), "unknown dep must fail validation");
    }

    #[test]
    fn test_content_sha256_computation() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"

[[plan.steps]]
step_id = "s1"
description = "step 1"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = []
"#;
        let plan = toml::from_str::<Plan>(toml).expect("parse ok");
        let hash = plan.compute_content_sha256();
        // sha256 hex is 64 chars
        assert_eq!(hash.len(), 64);
        // Deterministic: same input → same hash
        let hash2 = plan.compute_content_sha256();
        assert_eq!(hash, hash2);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-common --lib coordination::plan::tests 2>&1 | tail -10`
Expected: Compilation errors — `Plan` struct doesn't exist yet.

- [ ] **Step 3: Write `mur-common/src/coordination/plan.rs` with the full implementation**

```rust
//! Plan schema (§4) — TOML deserialization, validation, content hashing.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{DeterminismMode, Phase};

/// Top-level plan — a directed acyclic graph of steps (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub plan: PlanHeader,
    pub steps: Vec<Step>,
}

/// Plan-level metadata and settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanHeader {
    /// Protocol version. This spec = `"0"`.
    pub version: String,
    /// Unique identifier for this plan instance.
    pub plan_id: Uuid,
    /// Human-readable goal.
    pub goal: String,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Publisher: `agent:<id>` or `human:<name>`.
    pub created_by: String,
    /// Total predicted LLM/compute cost in USD.
    pub budget_estimate_usd: f64,
    /// Determinism mode (§7).
    #[serde(default)]
    pub determinism: DeterminismMode,
    /// SHA-256 of the canonical plan serialization (excluding this field and signature).
    ///
    /// Set via [`Plan::compute_content_sha256`] after construction.
    #[serde(default)]
    pub content_sha256: String,
    /// Optional Ed25519 signature over canonical bytes.
    #[serde(default)]
    pub signature: Option<String>,
    /// Max escalation count before giving up (§8.4). Default 3.
    #[serde(default = "default_max_escalations")]
    pub max_escalations: u32,
}

fn default_max_escalations() -> u32 {
    3
}

/// A single step in the plan — one agent assignment (§4.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Stable identifier within the plan.
    pub step_id: String,
    /// Human-readable description of what this step does.
    pub description: String,
    /// Preferred agent manifest name (e.g. "code-review").
    pub agent_hint: String,
    /// SDLC phases for this step, in execution order.
    pub phases: Vec<Phase>,
    /// Shell command or `verify://` URI for the Verify Gateway (§6).
    pub verify_command: String,
    /// Step ids that must complete before this step starts.
    #[serde(default)]
    pub depends_on: Vec<String>,

    // ── Optional fields ──────────────────────────────────────────
    /// Per-step budget override.
    #[serde(default)]
    pub budget_estimate_usd: Option<f64>,
    /// Per-step timeout override (seconds).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Skill reference: `<name>@<version>` if this step executes a skill.
    #[serde(default)]
    pub skill_ref: Option<String>,
    /// Per-step determinism override (inherits plan.determinism if absent).
    #[serde(default)]
    pub determinism: Option<DeterminismMode>,
    /// Step-specific input variables (free-form).
    #[serde(default)]
    pub input: Option<toml::Table>,
}

/// Validation error returned by [`Plan::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanValidationError {
    /// A step references a step_id that doesn't exist in the plan.
    UnknownDependency {
        step_id: String,
        referenced: String,
    },
    /// A cycle was detected in the step dependency graph.
    CycleDetected {
        cycle: Vec<String>,
    },
    /// A step has an empty phases list.
    EmptyPhases {
        step_id: String,
    },
    /// A step has an empty verify_command.
    MissingVerifyCommand {
        step_id: String,
    },
    /// Phases within a step are not in SDLC order.
    PhasesOutOfOrder {
        step_id: String,
        phases: Vec<Phase>,
    },
    /// content_sha256 is empty (call compute_content_sha256 first).
    MissingContentHash,
}

impl std::fmt::Display for PlanValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanValidationError::UnknownDependency {
                step_id,
                referenced,
            } => write!(
                f,
                "step '{}' depends on unknown step '{}'",
                step_id, referenced
            ),
            PlanValidationError::CycleDetected { cycle } => {
                write!(f, "dependency cycle detected: {}", cycle.join(" → "))
            }
            PlanValidationError::EmptyPhases { step_id } => {
                write!(f, "step '{}' has no phases", step_id)
            }
            PlanValidationError::MissingVerifyCommand { step_id } => {
                write!(f, "step '{}' has empty verify_command", step_id)
            }
            PlanValidationError::PhasesOutOfOrder { step_id, phases } => {
                write!(
                    f,
                    "step '{}' phases not in SDLC order: {:?}",
                    step_id, phases
                )
            }
            PlanValidationError::MissingContentHash => {
                write!(f, "content_sha256 is empty — call compute_content_sha256() first")
            }
        }
    }
}

impl Plan {
    /// Validate the plan structure. Returns Ok(()) or a list of errors.
    ///
    /// Checks:
    /// 1. All depends_on references are valid step_ids.
    /// 2. No dependency cycles.
    /// 3. Every step has at least one phase.
    /// 4. Every step has a non-empty verify_command.
    /// 5. Phases within a step are in SDLC order.
    /// 6. content_sha256 is non-empty.
    pub fn validate(&self) -> Result<(), Vec<PlanValidationError>> {
        let mut errors = Vec::new();
        let step_ids: std::collections::HashSet<&str> =
            self.steps.iter().map(|s| s.step_id.as_str()).collect();

        // Check 1 + 2: dependency graph
        for step in &self.steps {
            for dep in &step.depends_on {
                if !step_ids.contains(dep.as_str()) {
                    errors.push(PlanValidationError::UnknownDependency {
                        step_id: step.step_id.clone(),
                        referenced: dep.clone(),
                    });
                }
            }
        }
        if let Some(cycle) = detect_cycle(&self.steps) {
            errors.push(PlanValidationError::CycleDetected { cycle });
        }

        // Check 3: non-empty phases
        for step in &self.steps {
            if step.phases.is_empty() {
                errors.push(PlanValidationError::EmptyPhases {
                    step_id: step.step_id.clone(),
                });
            }
        }

        // Check 4: non-empty verify_command
        for step in &self.steps {
            if step.verify_command.trim().is_empty() {
                errors.push(PlanValidationError::MissingVerifyCommand {
                    step_id: step.step_id.clone(),
                });
            }
        }

        // Check 5: phases in order
        for step in &self.steps {
            let mut prev_idx: Option<u8> = None;
            for phase in &step.phases {
                let idx = phase.sdlc_index();
                if let Some(p) = prev_idx {
                    if idx <= p {
                        errors.push(PlanValidationError::PhasesOutOfOrder {
                            step_id: step.step_id.clone(),
                            phases: step.phases.clone(),
                        });
                        break; // one error per step for phase ordering
                    }
                }
                prev_idx = Some(idx);
            }
        }

        // Check 6: content hash present
        if self.plan.content_sha256.is_empty() {
            errors.push(PlanValidationError::MissingContentHash);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Compute the SHA-256 of the canonical TOML serialization.
    ///
    /// The canonical form strips `content_sha256` and `signature` fields,
    /// then serializes with sorted keys. This makes hashing deterministic
    /// regardless of field ordering or whitespace differences.
    pub fn compute_content_sha256(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = self.canonical_toml_bytes();
        hex::encode(Sha256::digest(&canonical))
    }

    /// Serialize the plan to canonical TOML bytes (for hashing).
    ///
    /// This strips `content_sha256` and `signature`, then uses
    /// `toml::to_string_pretty`. TOML serialization is deterministic
    /// for the same struct with the same field ordering.
    fn canonical_toml_bytes(&self) -> Vec<u8> {
        // Build a clean copy without the hash/sig fields for canonical output
        let clean = CleanPlan {
            plan: CleanPlanHeader {
                version: self.plan.version.clone(),
                plan_id: self.plan.plan_id,
                goal: self.plan.goal.clone(),
                created_at: self.plan.created_at.clone(),
                created_by: self.plan.created_by.clone(),
                budget_estimate_usd: self.plan.budget_estimate_usd,
                determinism: self.plan.determinism,
                max_escalations: self.plan.max_escalations,
            },
            steps: self
                .steps
                .iter()
                .map(|s| CleanStep {
                    step_id: s.step_id.clone(),
                    description: s.description.clone(),
                    agent_hint: s.agent_hint.clone(),
                    phases: s.phases.clone(),
                    verify_command: s.verify_command.clone(),
                    depends_on: s.depends_on.clone(),
                    budget_estimate_usd: s.budget_estimate_usd,
                    timeout_secs: s.timeout_secs,
                    skill_ref: s.skill_ref.clone(),
                    determinism: s.determinism,
                    input: s.input.clone(),
                })
                .collect(),
        };
        toml::to_string(&clean).unwrap().into_bytes()
    }
}

// ── Internal clean types for canonical serialization ──────────────

#[derive(Debug, Clone, Serialize)]
struct CleanPlan {
    plan: CleanPlanHeader,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    steps: Vec<CleanStep>,
}

#[derive(Debug, Clone, Serialize)]
struct CleanPlanHeader {
    version: String,
    plan_id: Uuid,
    goal: String,
    created_at: String,
    created_by: String,
    budget_estimate_usd: f64,
    determinism: DeterminismMode,
    max_escalations: u32,
}

#[derive(Debug, Clone, Serialize)]
struct CleanStep {
    step_id: String,
    description: String,
    agent_hint: String,
    phases: Vec<Phase>,
    verify_command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_estimate_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    skill_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    determinism: Option<DeterminismMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<toml::Table>,
}

// ── Cycle detection (Kahn's algorithm) ────────────────────────────

fn detect_cycle(steps: &[Step]) -> Option<Vec<String>> {
    use std::collections::{HashMap, VecDeque};

    let step_ids: Vec<&str> = steps.iter().map(|s| s.step_id.as_str()).collect();
    let id_to_idx: HashMap<&str, usize> = step_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    let n = steps.len();
    let mut in_degree = vec![0u32; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, step) in steps.iter().enumerate() {
        for dep in &step.depends_on {
            if let Some(&j) = id_to_idx.get(dep.as_str()) {
                adj[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut sorted = Vec::new();

    while let Some(u) = queue.pop_front() {
        sorted.push(u);
        for &v in &adj[u] {
            in_degree[v] -= 1;
            if in_degree[v] == 0 {
                queue.push_back(v);
            }
        }
    }

    if sorted.len() < n {
        // A cycle exists. Collect the remaining nodes for the error message.
        let cycle_nodes: Vec<String> = (0..n)
            .filter(|i| in_degree[*i] > 0)
            .map(|i| step_ids[i].to_string())
            .collect();
        Some(cycle_nodes)
    } else {
        None
    }
}
```

- [ ] **Step 4: Run the plan tests**

Run: `cargo test -p mur-common --lib coordination::plan::tests 2>&1 | tail -15`
Expected: All 8 tests pass.

- [ ] **Step 5: Run all coordination tests together**

Run: `cargo test -p mur-common --lib coordination:: 2>&1 | tail -10`
Expected: All 16 tests pass (8 types + 8 plan).

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/coordination/plan.rs
git commit -m "feat(coordination): add Plan/Step TOML schema with validation

M0 — Plan and Step types with serde TOML deser, Plan::validate()
(DAG acyclicity, phase ordering, verify_command presence, dep checks),
content_sha256 via canonical TOML bytes, Kahn's algorithm cycle detection.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Conformance adapter trait and test suite runner

**Files:**
- Create: `mur-common/src/coordination/conformance.rs`

- [ ] **Step 1: Write the failing test for the PlanLoadingSuite**

In `mur-common/src/coordination/conformance.rs`, write only a test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::plan::Plan;
    use crate::coordination::types::ConformanceLevel;

    /// A no-op adapter that returns pre-parsed plans from a Vec.
    struct StaticAdapter {
        plans: Vec<Plan>,
    }

    impl ConformanceAdapter for StaticAdapter {
        fn load_plan_toml(&self, _toml: &str) -> Result<Plan, String> {
            // Not called by the plan-loading suite — see plan_loading_suite design
            unimplemented!("adapter-level load_plan_toml not used by suite directly")
        }

        fn parse_and_validate(&self, toml: &str) -> Result<Plan, Vec<String>> {
            let plan: Plan = toml::from_str(toml).map_err(|e| vec![e.to_string()])?;
            plan.validate().map_err(|errs| errs.iter().map(|e| e.to_string()).collect())?;
            Ok(plan)
        }

        fn conformance_level(&self) -> ConformanceLevel {
            ConformanceLevel::Minimal
        }

        fn host_name(&self) -> &str {
            "static-test-adapter"
        }
    }

    #[test]
    fn test_conformance_minimal_plan_loading_passes() {
        let adapter = StaticAdapter { plans: vec![] };
        let suite = PlanLoadingSuite::new();
        let report = suite.run(&adapter);
        assert!(
            report.failures.is_empty(),
            "all plan-loading tests must pass: {:?}",
            report.failures
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common --lib coordination::conformance::tests 2>&1 | tail -10`
Expected: Compilation errors — `ConformanceAdapter` trait doesn't exist.

- [ ] **Step 3: Write `mur-common/src/coordination/conformance.rs`**

```rust
//! Conformance testing framework (§10).
//!
//! Hosts implement [`ConformanceAdapter`] and pass [`PlanLoadingSuite`]
//! (and future suites for Standard/Full levels) to prove conformance.
//!
//! The 10 plan-loading tests cover:
//! 1. Valid minimal plan
//! 2. Valid multi-step plan with dependencies
//! 3. Optional fields default correctly
//! 4. Unknown phase rejected
//! 5. Empty phases rejected
//! 6. Empty verify_command rejected
//! 7. Unknown dependency rejected
//! 8. Dependency cycle rejected
//! 9. Phases out of SDLC order rejected
//! 10. content_sha256 non-empty on valid plan

use serde::{Deserialize, Serialize};
use std::fmt;

use super::plan::Plan;
use super::types::ConformanceLevel;

/// Interface a host must implement to run the conformance suite.
///
/// The adapter is the **host's** bridge to the shared types. It provides:
/// - A TOML parser + validator (usually just `toml::from_str` + `Plan::validate`).
/// - The host's declared conformance level.
/// - A human-readable host name for test reports.
pub trait ConformanceAdapter {
    /// Parse a TOML string into a Plan and validate it.
    ///
    /// Returns the parsed Plan on success, or a list of human-readable
    /// error messages on failure.
    fn parse_and_validate(&self, toml: &str) -> Result<Plan, Vec<String>>;

    /// The conformance level this host claims.
    fn conformance_level(&self) -> ConformanceLevel;

    /// Human-readable host name for test reports.
    fn host_name(&self) -> &str;
}

/// Result of running a conformance suite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConformanceReport {
    /// Name of the suite that was run.
    pub suite_name: String,
    /// Host name (from [`ConformanceAdapter::host_name`]).
    pub host_name: String,
    /// Total tests in the suite.
    pub total: usize,
    /// Number of tests that passed.
    pub passed: usize,
    /// Number of tests that failed.
    pub failed: usize,
    /// Per-failure details (empty if all passed).
    pub failures: Vec<TestFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    pub test_name: String,
    pub error: String,
}

impl ConformanceReport {
    /// Did every test in the suite pass?
    pub fn all_passed(&self) -> bool {
        self.failures.is_empty()
    }
}

impl fmt::Display for ConformanceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} — {}: {}/{} passed",
            self.suite_name, self.host_name, self.passed, self.total
        )?;
        for failure in &self.failures {
            writeln!(f, "  FAIL {}: {}", failure.test_name, failure.error)?;
        }
        Ok(())
    }
}

/// The Minimal-conformance plan-loading test suite.
///
/// Contains 10 self-contained tests that exercise Plan TOML parsing
/// and validation. Each test is a TOML string + an assertion about
/// whether it should parse+validate successfully.
pub struct PlanLoadingSuite {
    cases: Vec<PlanLoadingCase>,
}

struct PlanLoadingCase {
    name: &'static str,
    toml: &'static str,
    should_pass: bool,
    /// If should_pass, also verify the parsed plan has the expected field values.
    check_fields: Option<Box<dyn Fn(&Plan) -> Result<(), String>>>,
}

impl PlanLoadingSuite {
    pub fn new() -> Self {
        Self {
            cases: vec![
                // 1. Valid minimal plan
                PlanLoadingCase {
                    name: "valid_minimal_plan",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "a"  # validation requires non-empty hash

[[plan.steps]]
step_id = "s1"
description = "test"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = []
"#,
                    should_pass: true,
                    check_fields: None,
                },
                // 2. Valid multi-step plan with dependencies
                PlanLoadingCase {
                    name: "valid_multi_step_with_deps",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "multi-step"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 1.00
determinism = "strict"
content_sha256 = "b"

[[plan.steps]]
step_id = "build"
description = "build the project"
agent_hint = "generic"
phases = ["plan", "implement", "verify"]
verify_command = "cargo build"
depends_on = []

[[plan.steps]]
step_id = "test"
description = "run tests"
agent_hint = "code-review"
phases = ["test", "verify"]
verify_command = "cargo test"
depends_on = ["build"]

[[plan.steps]]
step_id = "deploy"
description = "deploy"
agent_hint = "generic"
phases = ["verify"]
verify_command = "curl localhost/health"
depends_on = ["test"]
"#,
                    should_pass: true,
                    check_fields: None,
                },
                // 3. Optional fields default correctly
                PlanLoadingCase {
                    name: "optional_fields_default",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "test defaults"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "c"

[[plan.steps]]
step_id = "s1"
description = "test"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = []
"#,
                    should_pass: true,
                    check_fields: Some(Box::new(|plan: &Plan| {
                        if plan.plan.signature.is_some() {
                            return Err("signature should be None by default".into());
                        }
                        if plan.steps[0].skill_ref.is_some() {
                            return Err("skill_ref should be None by default".into());
                        }
                        if plan.plan.max_escalations != 3 {
                            return Err(format!(
                                "max_escalations should default to 3, got {}",
                                plan.plan.max_escalations
                            ));
                        }
                        Ok(())
                    })),
                },
                // 4. Unknown phase rejected
                PlanLoadingCase {
                    name: "reject_unknown_phase",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "bad phase"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "d"

[[plan.steps]]
step_id = "s1"
description = "bad phase"
agent_hint = "generic"
phases = ["bogus_phase"]
verify_command = "true"
depends_on = []
"#,
                    should_pass: false,
                    check_fields: None,
                },
                // 5. Empty phases rejected
                PlanLoadingCase {
                    name: "reject_empty_phases",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "no phases"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "e"

[[plan.steps]]
step_id = "s1"
description = "no phases"
agent_hint = "generic"
phases = []
verify_command = "true"
depends_on = []
"#,
                    should_pass: false,
                    check_fields: None,
                },
                // 6. Empty verify_command rejected
                PlanLoadingCase {
                    name: "reject_empty_verify_command",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "no verify"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "f"

[[plan.steps]]
step_id = "s1"
description = "no verify"
agent_hint = "generic"
phases = ["verify"]
verify_command = ""
depends_on = []
"#,
                    should_pass: false,
                    check_fields: None,
                },
                // 7. Unknown dependency rejected
                PlanLoadingCase {
                    name: "reject_unknown_dependency",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "bad dep"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "g"

[[plan.steps]]
step_id = "s1"
description = "bad dep"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = ["nonexistent"]
"#,
                    should_pass: false,
                    check_fields: None,
                },
                // 8. Dependency cycle rejected
                PlanLoadingCase {
                    name: "reject_dependency_cycle",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "cycle"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "h"

[[plan.steps]]
step_id = "s1"
description = "step 1"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = ["s2"]

[[plan.steps]]
step_id = "s2"
description = "step 2"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = ["s1"]
"#,
                    should_pass: false,
                    check_fields: None,
                },
                // 9. Phases out of SDLC order rejected
                PlanLoadingCase {
                    name: "reject_phases_out_of_order",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "bad order"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "i"

[[plan.steps]]
step_id = "s1"
description = "bad order"
agent_hint = "generic"
phases = ["verify", "plan", "implement"]
verify_command = "true"
depends_on = []
"#,
                    should_pass: false,
                    check_fields: None,
                },
                // 10. content_sha256 must be non-empty
                PlanLoadingCase {
                    name: "reject_empty_content_hash",
                    toml: r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "no hash"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = ""

[[plan.steps]]
step_id = "s1"
description = "test"
agent_hint = "generic"
phases = ["verify"]
verify_command = "true"
depends_on = []
"#,
                    should_pass: false,
                    check_fields: None,
                },
            ],
        }
    }

    /// Run all 10 tests against the given adapter.
    pub fn run(&self, adapter: &dyn ConformanceAdapter) -> ConformanceReport {
        let mut failures = Vec::new();

        for case in &self.cases {
            let result = adapter.parse_and_validate(case.toml);
            match (case.should_pass, result) {
                (true, Ok(ref plan)) => {
                    if let Some(ref check) = case.check_fields {
                        if let Err(err) = check(plan) {
                            failures.push(TestFailure {
                                test_name: case.name.to_string(),
                                error: format!("field check failed: {}", err),
                            });
                        }
                    }
                }
                (true, Err(errs)) => {
                    failures.push(TestFailure {
                        test_name: case.name.to_string(),
                        error: format!("expected success, got errors: {:?}", errs),
                    });
                }
                (false, Ok(_)) => {
                    failures.push(TestFailure {
                        test_name: case.name.to_string(),
                        error: "expected validation failure, got success".to_string(),
                    });
                }
                (false, Err(_)) => {
                    // Expected failure — correct.
                }
            }
        }

        let total = self.cases.len();
        let failed_count = failures.len();
        ConformanceReport {
            suite_name: "PlanLoadingSuite".to_string(),
            host_name: adapter.host_name().to_string(),
            total,
            passed: total - failed_count,
            failed: failed_count,
            failures,
        }
    }
}

impl Default for PlanLoadingSuite {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Run the conformance tests**

Run: `cargo test -p mur-common --lib coordination::conformance::tests 2>&1 | tail -10`
Expected: 1 test passes (the `StaticAdapter` test exercises all 10 cases).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/coordination/conformance.rs
git commit -m "feat(coordination): add ConformanceAdapter trait and PlanLoadingSuite

M0 — ConformanceAdapter trait for hosts to implement, PlanLoadingSuite
with 10 self-contained test cases (valid plans, defaults, rejection of
bad phases/deps/cycles/hash), ConformanceReport with Display.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: Integration conformance tests

**Files:**
- Create: `mur-common/tests/coordination_conformance.rs`

- [ ] **Step 1: Write the integration test**

Create `mur-common/tests/coordination_conformance.rs`:

```rust
//! Integration test: mur-common itself passes the PlanLoadingSuite.
//!
//! This proves the shared types work end-to-end with no host-specific
//! adapter required. Hosts (mur-commander, mur-runtime) write their
//! own integration tests that import this same suite and run it against
//! their adapter.

use mur_common::coordination::conformance::{ConformanceAdapter, PlanLoadingSuite};
use mur_common::coordination::plan::Plan;
use mur_common::coordination::types::ConformanceLevel;

/// mur-common's own adapter — uses the types directly.
struct MurCommonAdapter;

impl ConformanceAdapter for MurCommonAdapter {
    fn parse_and_validate(&self, toml: &str) -> Result<Plan, Vec<String>> {
        let plan: Plan = toml::from_str(toml).map_err(|e| vec![e.to_string()])?;
        plan.validate()
            .map_err(|errs| errs.iter().map(|e| e.to_string()).collect())?;
        Ok(plan)
    }

    fn conformance_level(&self) -> ConformanceLevel {
        ConformanceLevel::Minimal
    }

    fn host_name(&self) -> &str {
        "mur-common-self"
    }
}

#[test]
fn test_mur_common_passes_plan_loading_suite() {
    let adapter = MurCommonAdapter;
    let suite = PlanLoadingSuite::new();
    let report = suite.run(&adapter);
    assert!(
        report.all_passed(),
        "mur-common must pass all plan-loading tests:\n{}",
        report
    );
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p mur-common --test coordination_conformance 2>&1 | tail -10`
Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add mur-common/tests/coordination_conformance.rs
git commit -m "test(coordination): add integration conformance test for mur-common

M0 — mur-common passes PlanLoadingSuite via MurCommonAdapter.
Hosts copy this pattern with their own adapters.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Full workspace check + documentation

**Files:**
- Modify: `README.md` (mur root) — mention coordination module
- Modify: `~/Projects/mur-commander/README.md` — mention coordination protocol

- [ ] **Step 1: Run full mur workspace check**

Run: `cargo check --workspace 2>&1 | tail -5`
Expected: Compiles without errors (new module is additive, no existing code changed).

- [ ] **Step 2: Run full mur workspace tests**

Run: `cargo test --workspace --tests 2>&1 | tail -10`
Expected: All tests pass (no regressions from new module).

- [ ] **Step 3: Add coordination note to mur README**

In mur's `README.md`, find the "Architecture" or "Crates" section and add a bullet:

```markdown
- `mur-common/src/coordination/` — Shared coordination protocol types (Plan/Step/Microstep schema, Verify Gateway, conformance suite). Used by mur-runtime and mur-commander.
```

- [ ] **Step 4: Verify commander compiles with current mur-common tag (no-op check)**

Commander currently pins `mur-common` at `v2.18.0`. The new module does not yet exist at that tag, so commander will NOT auto-pick it up until the tag is bumped. This is the correct behavior — coordination types arrive in commander when it bumps to a mur release that includes M0.

Run (in commander repo): `cargo check -p mur-engine 2>&1 | tail -5`
Expected: Compiles without errors (commander has no coordination imports yet).

- [ ] **Step 5: Write commander's conformance test scaffold (does not compile yet — commented-out template)**

Create `~/Projects/mur-commander/crates/engine/tests/coordination_conformance.rs` as a commented-out template:

```rust
//! Commander coordination conformance test — uncomment when mur-common
//! tag is bumped to include the coordination module (post-M0).
//!
//! ```bash
//! # To activate:
//! # 1. Bump mur-common tag in Cargo.toml to >= v2.19.0 (or whichever includes M0)
//! # 2. Uncomment the code below
//! # 3. Run: cargo test -p mur-engine --test coordination_conformance
//! ```
//
// use mur_common::coordination::conformance::{ConformanceAdapter, PlanLoadingSuite};
// use mur_common::coordination::plan::Plan;
// use mur_common::coordination::types::ConformanceLevel;
//
// struct CommanderAdapter;
//
// impl ConformanceAdapter for CommanderAdapter {
//     fn parse_and_validate(&self, toml: &str) -> Result<Plan, Vec<String>> {
//         let plan: Plan = toml::from_str(toml).map_err(|e| vec![e.to_string()])?;
//         plan.validate()
//             .map_err(|errs| errs.iter().map(|e| e.to_string()).collect())?;
//         Ok(plan)
//     }
//
//     fn conformance_level(&self) -> ConformanceLevel {
//         ConformanceLevel::Minimal
//     }
//
//     fn host_name(&self) -> &str {
//         "mur-commander"
//     }
// }
//
// #[test]
// fn test_commander_passes_plan_loading_suite() {
//     let adapter = CommanderAdapter;
//     let suite = PlanLoadingSuite::new();
//     let report = suite.run(&adapter);
//     assert!(
//         report.all_passed(),
//         "commander must pass all plan-loading tests:\n{}",
//         report
//     );
// }
```

- [ ] **Step 6: Commit (mur repo)**

```bash
git add README.md
git commit -m "docs: mention coordination protocol module in README

M0 documentation — mur-common/src/coordination/ is the shared source
of truth for Plan/Step/Microstep schema across mur-runtime and commander.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 7: Commit (commander repo)**

```bash
git add crates/engine/tests/coordination_conformance.rs
git commit -m "test: add coordination conformance test scaffold (commented out)

M0 prep — commander conformance test template. Activates when
mur-common tag is bumped to include the coordination module.
Currently commented out to avoid compile errors on v2.18.0.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Post-M0 Verification Checklist

Before declaring M0 complete, verify:

1. `cargo test -p mur-common --lib coordination::` — All unit tests pass (≥ 17 tests across types/plan/conformance).
2. `cargo test -p mur-common --test coordination_conformance` — Integration conformance test passes.
3. `cargo doc -p mur-common --no-deps --open` — `mur_common::coordination` module appears with all public types documented.
4. `cargo check --workspace` — No regressions in any mur workspace crate.
5. `cargo check -p mur-engine` (commander repo) — Still compiles (no coordination imports yet, no breakage).
6. `PlanLoadingSuite::new().run(&adapter)` returns `ConformanceReport { passed: 10, failed: 0 }`.
7. A hand-written plan TOML file can be parsed by `toml::from_str::<Plan>()` and passes `validate()`.

---

## What Comes After M0

M0 delivers the **shared vocabulary and conformance contract**. No runtime behavior.

| Milestone | What it builds on M0 | Timeline |
|---|---|---|
| **M1** (Commander P2) | Commander implements `ConformanceAdapter`, wires Plan parser into `plan_executor.rs`, reaches Standard conformance | +4 weeks |
| **M2** (mur-runtime) | mur-runtime implements `ConformanceAdapter`, wires Plan parser into `task_runner.rs`, reaches Standard conformance | +4 weeks (parallel to M1) |
| **M3** (Replay + Full) | Both hosts reach Full conformance with idempotency-key enforcement | +4 weeks |

Each host's conformance test imports `PlanLoadingSuite` from `mur_common::coordination::conformance` and runs it against that host's adapter — the same suite, two adapters, proven interoperable.

---

## Implementation Order Summary

| Seq | Task | New Files | Tests |
|-----|------|-----------|-------|
| 1 | Add `toml` dependency | 0 | 0 |
| 2 | Type enums | 2 (mod.rs, types.rs) | 8 |
| 3 | Plan/Step schema + validation | 1 (plan.rs) | 8 |
| 4 | Conformance adapter + suite | 1 (conformance.rs) | 1 |
| 5 | Integration conformance test | 1 (tests/coordination_conformance.rs) | 1 |
| 6 | Docs + commander scaffold | 1 (commander test template) | 0 |

Total: 6 tasks, 6 new files, 1 modified dependency file, 18+ tests, ~800 lines of Rust.
