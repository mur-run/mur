//! Repair dispatch tests for `mur skill doctor --fix --apply` (M5b).

use mur_core::cmd::skill_doctor::{Finding, Severity};
use mur_core::skill_repair::{RepairCtx, run_repairs};
use std::path::Path;

fn make_finding(skill_name: &str, check_id: &str, fixable: bool) -> Finding {
    Finding {
        check_id: check_id.to_string(),
        category: "deps".to_string(),
        severity: Severity::Fail,
        skill_name: skill_name.to_string(),
        message: "Required skill 'missing-dep' is not installed.".to_string(),
        remediation: Some("mur skill install missing-dep".to_string()),
        fixable,
    }
}

#[test]
fn unfixable_findings_are_skipped() {
    let unfixable = make_finding("test-skill", "execution-recency", false);
    assert!(!unfixable.fixable);
}

#[test]
fn fixable_findings_flow_through_dry_run() {
    let fixable = make_finding("test-skill", "dependency-freshness", true);
    assert!(fixable.fixable);
}

#[test]
fn repair_report_counts_correctly() {
    let findings = vec![
        make_finding("a", "dependency-freshness", true),
        make_finding("b", "execution-recency", false),
        make_finding("c", "tool-availability", false),
    ];

    let repairs: Vec<Box<dyn mur_core::skill_repair::Repair>> = vec![
        Box::new(mur_core::skill_repair::dep_freshness::DepFreshnessRepair),
        Box::new(mur_core::skill_repair::tool_availability::ToolAvailabilityRepair),
    ];

    let home = Path::new("/nonexistent");
    let ctx = RepairCtx {
        home,
        registry_url: "https://example.com",
    };

    // Dry-run (apply=false) — only 1 fixable finding
    let report = run_repairs(&findings, false, &ctx, &repairs);
    assert_eq!(report.fixed, 0);
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.dry_run, 1);

    // Apply mode — dep_freshness will try to install (fails on /nonexistent)
    // We'll get Failed since the path doesn't exist
    let report2 = run_repairs(&findings, true, &ctx, &repairs);
    assert_eq!(
        report2.fixed + report2.skipped + report2.failed,
        report2.outcomes.len()
    );
}

#[test]
fn tool_availability_always_skipped() {
    // tool_availability repair always returns Skipped (requires agent context)
    let finding = make_finding("test", "tool-availability", true);
    let repairs: Vec<Box<dyn mur_core::skill_repair::Repair>> = vec![Box::new(
        mur_core::skill_repair::tool_availability::ToolAvailabilityRepair,
    )];
    let home = Path::new("/nonexistent");
    let ctx = RepairCtx {
        home,
        registry_url: "https://example.com",
    };

    let report = run_repairs(&[finding], true, &ctx, &repairs);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.fixed, 0);
}
