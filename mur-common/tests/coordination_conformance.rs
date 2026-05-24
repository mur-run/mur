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
