//! Repair engine for `mur skill doctor --fix` (M5b).
//!
//! Each fixable check implements the `Repair` trait. The dispatcher
//! (`run_repairs`) finds the first matching repair for each finding and
//! runs it. When `--apply` is absent all outcomes are `DryRun`.

use std::path::Path;

use crate::cmd::skill_doctor::Finding;

pub mod dep_freshness;
pub mod stats_sidecar;
pub mod tool_availability;

pub enum RepairOutcome {
    Fixed,
    Skipped(String),
    Failed(anyhow::Error),
    DryRun(String),
}

pub struct RepairOutcomeMsg {
    pub skill_name: String,
    pub check_id: String,
    pub outcome: RepairOutcome,
}

pub struct RepairReport {
    pub fixed: usize,
    pub skipped: usize,
    pub dry_run: usize,
    pub failed: usize,
    pub outcomes: Vec<RepairOutcomeMsg>,
}

pub struct RepairCtx<'a> {
    pub home: &'a Path,
    pub registry_url: &'a str,
}

pub trait Repair {
    #[allow(dead_code)]
    fn check_id(&self) -> &'static str;
    fn applicable(&self, finding: &Finding) -> bool;
    fn run(&self, finding: &Finding, ctx: &RepairCtx, apply: bool) -> RepairOutcome;
}

pub fn run_repairs(
    findings: &[Finding],
    apply: bool,
    ctx: &RepairCtx,
    repairs: &[Box<dyn Repair>],
) -> RepairReport {
    let mut report = RepairReport {
        fixed: 0,
        skipped: 0,
        dry_run: 0,
        failed: 0,
        outcomes: Vec::new(),
    };

    for finding in findings {
        if !finding.fixable {
            continue;
        }
        let Some(repair) = repairs.iter().find(|r| r.applicable(finding)) else {
            continue;
        };

        let outcome = if apply {
            repair.run(finding, ctx, true)
        } else {
            let dry = repair.run(finding, ctx, false);
            match dry {
                RepairOutcome::DryRun(_) => dry,
                RepairOutcome::Fixed => RepairOutcome::DryRun("would fix".into()),
                RepairOutcome::Skipped(_) => dry,
                RepairOutcome::Failed(_) => dry,
            }
        };

        match &outcome {
            RepairOutcome::Fixed => report.fixed += 1,
            RepairOutcome::Skipped(_) => report.skipped += 1,
            RepairOutcome::DryRun(_) => report.dry_run += 1,
            RepairOutcome::Failed(_) => report.failed += 1,
        }

        report.outcomes.push(RepairOutcomeMsg {
            skill_name: finding.skill_name.clone(),
            check_id: finding.check_id.clone(),
            outcome,
        });
    }

    report
}

pub fn print_repair_summary(report: &RepairReport, apply: bool) {
    let mode = if apply { "Applied" } else { "Dry-run" };
    println!("Repair summary ({mode}):");
    for msg in &report.outcomes {
        match &msg.outcome {
            RepairOutcome::Fixed => {
                println!("  Fixed:     {} ({})", msg.skill_name, msg.check_id);
            }
            RepairOutcome::DryRun(detail) => {
                println!(
                    "  DryRun:    {} ({}) — {detail}",
                    msg.skill_name, msg.check_id
                );
            }
            RepairOutcome::Skipped(detail) => {
                println!(
                    "  Skipped:   {} ({}) — {detail}",
                    msg.skill_name, msg.check_id
                );
            }
            RepairOutcome::Failed(e) => {
                println!("  Failed:    {} ({}) — {e}", msg.skill_name, msg.check_id);
            }
        }
    }
    println!(
        "  Fixed: {}  Skipped: {}  DryRun: {}  Failed: {}",
        report.fixed, report.skipped, report.dry_run, report.failed
    );
}
