//! Plan schema (§4) — TOML deserialization, validation, content hashing.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{DeterminismMode, Phase};

/// Top-level plan — a directed acyclic graph of steps (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub plan: PlanHeader,
}

/// Plan-level metadata and settings, including the step list (§4).
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
    #[serde(default)]
    pub content_sha256: String,
    /// Optional Ed25519 signature over canonical bytes.
    #[serde(default)]
    pub signature: Option<String>,
    /// Max escalation count before giving up (§8.4). Default 3.
    #[serde(default = "default_max_escalations")]
    pub max_escalations: u32,
    /// Ordered list of steps forming the plan DAG.
    #[serde(default)]
    pub steps: Vec<Step>,
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
    /// Whether to run an independent judge on this step's result.
    #[serde(default)]
    pub judge: bool,
}

/// Validation error returned by [`Plan::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanValidationError {
    UnknownDependency {
        step_id: String,
        referenced: String,
    },
    CycleDetected {
        cycle: Vec<String>,
    },
    EmptyPhases {
        step_id: String,
    },
    MissingVerifyCommand {
        step_id: String,
    },
    PhasesOutOfOrder {
        step_id: String,
        phases: Vec<Phase>,
    },
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
    /// Validate the plan structure. Returns a list of errors (empty = valid).
    pub fn validate(&self) -> Vec<PlanValidationError> {
        let mut errors = Vec::new();
        let step_ids: std::collections::HashSet<&str> =
            self.plan.steps.iter().map(|s| s.step_id.as_str()).collect();

        // Check 1 + 2: dependency graph
        for step in &self.plan.steps {
            for dep in &step.depends_on {
                if !step_ids.contains(dep.as_str()) {
                    errors.push(PlanValidationError::UnknownDependency {
                        step_id: step.step_id.clone(),
                        referenced: dep.clone(),
                    });
                }
            }
        }
        if let Some(cycle) = detect_cycle(&self.plan.steps) {
            errors.push(PlanValidationError::CycleDetected { cycle });
        }

        // Check 3: non-empty phases
        for step in &self.plan.steps {
            if step.phases.is_empty() {
                errors.push(PlanValidationError::EmptyPhases {
                    step_id: step.step_id.clone(),
                });
            }
        }

        // Check 4: non-empty verify_command
        for step in &self.plan.steps {
            if step.verify_command.trim().is_empty() {
                errors.push(PlanValidationError::MissingVerifyCommand {
                    step_id: step.step_id.clone(),
                });
            }
        }

        // Check 5: phases in order
        for step in &self.plan.steps {
            let mut prev_idx: Option<u8> = None;
            for phase in &step.phases {
                let idx = phase.sdlc_index();
                if let Some(p) = prev_idx {
                    if idx <= p {
                        errors.push(PlanValidationError::PhasesOutOfOrder {
                            step_id: step.step_id.clone(),
                            phases: step.phases.clone(),
                        });
                        break;
                    }
                }
                prev_idx = Some(idx);
            }
        }

        // Check 6: content hash present
        if self.plan.content_sha256.is_empty() {
            errors.push(PlanValidationError::MissingContentHash);
        }

        errors
    }

    /// Compute the SHA-256 of the canonical TOML serialization.
    pub fn compute_content_sha256(&self) -> String {
        use sha2::{Digest, Sha256};
        let canonical = self.canonical_toml_bytes();
        hex::encode(Sha256::digest(&canonical))
    }

    /// Serialize the plan to canonical TOML bytes (for hashing).
    /// This strips `content_sha256` and `signature`.
    fn canonical_toml_bytes(&self) -> Vec<u8> {
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
                steps: self
                    .plan
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
            },
        };
        toml::to_string(&clean).unwrap().into_bytes()
    }
}

// ── Internal clean types for canonical serialization ──────────────

#[derive(Debug, Clone, Serialize)]
struct CleanPlan {
    plan: CleanPlanHeader,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    steps: Vec<CleanStep>,
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
        let cycle_nodes: Vec<String> = (0..n)
            .filter(|i| in_degree[*i] > 0)
            .map(|i| step_ids[i].to_string())
            .collect();
        Some(cycle_nodes)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::types::*;
    use uuid::Uuid;

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
            "550e8400-e29b-41d4-a716-446655440000".parse::<Uuid>().unwrap()
        );
        assert_eq!(plan.plan.goal, "Add Stripe webhook handler");
        assert_eq!(plan.plan.determinism, DeterminismMode::BestEffort);
        assert_eq!(plan.plan.steps.len(), 2);
        assert_eq!(plan.plan.steps[0].step_id, "step_001");
        assert_eq!(plan.plan.steps[0].agent_hint, "code-review");
        assert_eq!(
            plan.plan.steps[0].phases,
            vec![
                Phase::Plan,
                Phase::Design,
                Phase::Implement,
                Phase::Test,
                Phase::Verify,
            ]
        );
        assert!(plan.plan.steps[0].depends_on.is_empty());
        assert_eq!(plan.plan.steps[1].depends_on, vec!["step_001".to_string()]);
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
        assert!(plan.plan.signature.is_none());
        assert!(plan.plan.steps[0].skill_ref.is_none());
        assert_eq!(plan.plan.steps[0].budget_estimate_usd, None);
        assert_eq!(plan.plan.steps[0].timeout_secs, None);
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
    fn test_reject_dependency_cycle() {
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
        let has_cycle = errors.iter().any(|e| matches!(e, PlanValidationError::CycleDetected { .. }));
        assert!(has_cycle, "error must mention cycle, got: {:?}", errors);
    }

    #[test]
    fn test_validate_valid_plan() {
        let toml = r#"
[plan]
version = "0"
plan_id = "550e8400-e29b-41d4-a716-446655440000"
goal = "valid plan"
created_at = "2026-05-24T12:00:00Z"
created_by = "agent:test"
budget_estimate_usd = 0.0
determinism = "best-effort"
content_sha256 = "abc123"

[[plan.steps]]
step_id = "build"
description = "build"
agent_hint = "generic"
phases = ["plan", "implement", "verify"]
verify_command = "cargo build"
depends_on = []

[[plan.steps]]
step_id = "test"
description = "test"
agent_hint = "code-review"
phases = ["test", "verify"]
verify_command = "cargo test"
depends_on = ["build"]
"#;
        let plan: Plan = toml::from_str(toml).expect("parse valid plan");
        let errors = plan.validate();
        assert!(errors.is_empty(), "validation must pass for valid plan, got: {:?}", errors);
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
        assert_eq!(hash.len(), 64);
        let hash2 = plan.compute_content_sha256();
        assert_eq!(hash, hash2);
    }
}
