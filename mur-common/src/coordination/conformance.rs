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
content_sha256 = "a"

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
                        if plan.plan.steps[0].skill_ref.is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::plan::Plan;
    use crate::coordination::types::ConformanceLevel;

    /// A no-op adapter that returns pre-parsed plans from a Vec.
    #[allow(dead_code)]
    struct StaticAdapter {
        plans: Vec<Plan>,
    }

    impl ConformanceAdapter for StaticAdapter {
        fn parse_and_validate(&self, toml: &str) -> Result<Plan, Vec<String>> {
            let plan: Plan = toml::from_str(toml).map_err(|e| vec![e.to_string()])?;
            let errors = plan.validate();
            if !errors.is_empty() {
                return Err(errors.iter().map(|e| e.to_string()).collect());
            }
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
        let adapter = StaticAdapter {
            plans: vec![],
        };
        let suite = PlanLoadingSuite::new();
        let report = suite.run(&adapter);
        assert!(
            report.failures.is_empty(),
            "all plan-loading tests must pass: {:?}",
            report.failures
        );
    }
}
