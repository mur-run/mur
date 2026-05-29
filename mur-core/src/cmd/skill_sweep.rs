//! `mur skill sweep` CLI handler (M5b).

use anyhow::Result;

use super::agent::resolve_mur_home;

pub fn cmd_sweep(filter: Option<&str>, dry_run: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let report = crate::skill_lifecycle::sweep::run_sweep(
        &home,
        crate::skill_lifecycle::sweep::SweepOptions {
            filter: filter.map(str::to_string),
            dry_run,
            now: chrono::Utc::now(),
            require_human_curation_before_stable: true,
        },
    )?;

    let dry_label = if dry_run { " (dry-run)" } else { "" };
    println!("Examined: {} skill(s){}", report.examined, dry_label);

    if !report.transitions.is_empty() {
        println!("Transitions ({}):", report.transitions.len());
        for t in &report.transitions {
            println!(
                "  {:20} {:?} -> {:?}    ({})",
                t.skill_name, t.from, t.to, t.reason
            );
        }
    } else {
        println!("Transitions: 0");
    }

    println!(
        "Decayed (read): {} (confidence recomputed on read; persisted only on promotion)",
        report.decayed
    );
    println!("Archived: {}", report.archived);

    if dry_run {
        println!("(dry-run; no changes written)");
    }

    Ok(())
}
