//! CLI dispatcher for `mur skill consolidate` (M5b).

use std::io::IsTerminal;
use std::path::Path;

use anyhow::Result;

use crate::skill_consolidate::{ConsolidateOptions, run_consolidate};

pub fn cmd_consolidate(home: &Path, dry_run: bool, apply: bool) -> Result<()> {
    if apply && std::io::stdin().is_terminal() {
        eprint!(
            "About to apply consolidation changes (archive orphans, deprecate duplicates). Continue? [y/N] "
        );
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim().to_lowercase() != "y" {
            println!("Aborted.");
            return Ok(());
        }
    }

    let opts = ConsolidateOptions {
        dry_run,
        apply: apply && !dry_run,
    };
    let report = run_consolidate(home, &opts)?;
    print_summary(&report, apply);
    Ok(())
}

fn print_summary(report: &crate::skill_consolidate::ConsolidateReport, applied: bool) {
    let mode = if applied { "Applied" } else { "Dry-run" };
    println!(
        "Consolidation report ({mode}): {} duplicate(s), {} contradiction(s), {} orphan(s)",
        report.duplicates.len(),
        report.contradictions.len(),
        report.orphans.len(),
    );

    for d in &report.duplicates {
        println!(
            "  Duplicate: {} ≈ {} (sim={:.3}, keeper={})",
            d.a, d.b, d.similarity, d.keeper,
        );
    }
    for c in &report.contradictions {
        println!(
            "  Contradiction: {} vs {} on trigger '{}' — {}",
            c.a, c.b, c.trigger, c.reason,
        );
    }
    for o in &report.orphans {
        let ago = o.last_used.map(|t| {
            let d = (chrono::Utc::now() - t).num_days();
            format!("{d}d ago")
        });
        println!(
            "  Orphan: {} (used {}x, last {})",
            o.name,
            o.usage_count,
            ago.as_deref().unwrap_or("never"),
        );
    }
}
