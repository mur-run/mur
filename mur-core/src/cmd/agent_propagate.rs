//! `mur agent propagate` CLI handler (M7c).

use std::path::Path;

use anyhow::Result;

use crate::cross_agent::propagate::{
    PropagateOptions, PropagateReport, candidates::GateConfig, run_propagate,
};

pub fn cmd_propagate(
    home: &Path,
    agent: &str,
    dry_run: bool,
    max: Option<usize>,
    min_fitness: Option<f64>,
    min_samples: Option<u64>,
    json: bool,
) -> Result<()> {
    let mut opts = PropagateOptions {
        gates: GateConfig::default(),
        dry_run,
    };
    if let Some(m) = max {
        opts.gates.max_per_sweep = m;
    }
    if let Some(f) = min_fitness {
        opts.gates.min_fitness = f;
    }
    if let Some(s) = min_samples {
        opts.gates.min_samples = s;
    }

    match run_propagate(home, agent, &opts) {
        Ok(report) => {
            if json {
                emit_json(&report)?;
            } else {
                emit_human(&report, &opts);
            }
            if !report.failed.is_empty() {
                std::process::exit(5);
            }
            if report.scanned_peers == 0 {
                std::process::exit(4);
            }
            Ok(())
        }
        Err(e) => {
            if e.to_string().contains("exit 7") {
                eprintln!("propagate already running — skipping");
                std::process::exit(7);
            }
            Err(e)
        }
    }
}

fn emit_human(report: &PropagateReport, opts: &PropagateOptions) {
    let g = &opts.gates;
    println!(
        "Scanned {} peers, found {} candidate skill(s).",
        report.scanned_peers,
        report.candidates.len()
    );
    println!(
        "Gates: min_samples={}  min_fitness={:.2}  min_source_weight={:.2}  max_per_sweep={}",
        g.min_samples, g.min_fitness, g.min_source_weight, g.max_per_sweep
    );
    println!();
    if opts.dry_run {
        println!("(dry-run)");
    }
    if !report.installed.is_empty() {
        println!("Installed ({}):", report.installed.len());
        for c in &report.installed {
            println!(
                "  {:<22} v{}  ← agent://{}  (fitness {:.2}, n={})",
                c.skill,
                c.source_version,
                c.source_agent,
                c.population_fitness,
                c.population_samples
            );
        }
        println!();
    }
    if !report.candidates.is_empty() && report.installed.len() < report.candidates.len() {
        let skipped: Vec<_> = report
            .candidates
            .iter()
            .filter(|c| !report.installed.iter().any(|i| i.skill == c.skill))
            .collect();
        if !skipped.is_empty() {
            println!(
                "Skipped ({}) — below gates or already present:",
                skipped.len()
            );
            for c in skipped {
                println!(
                    "  {:<22} v{}  ← agent://{}  (fitness {:.2}, n={})",
                    c.skill,
                    c.source_version,
                    c.source_agent,
                    c.population_fitness,
                    c.population_samples
                );
            }
            println!();
        }
    }
    if !report.failed.is_empty() {
        eprintln!("Failed ({}):", report.failed.len());
        for (c, msg) in &report.failed {
            eprintln!("  {:<22}  {msg}", c.skill);
        }
    }
}

fn emit_json(report: &PropagateReport) -> Result<()> {
    let obj = serde_json::json!({
        "scanned_peers": report.scanned_peers,
        "installed": report.installed.iter().map(|c| {
            serde_json::json!({
                "skill": c.skill,
                "source_agent": c.source_agent,
                "source_version": c.source_version,
                "population_fitness": c.population_fitness,
                "population_samples": c.population_samples,
            })
        }).collect::<Vec<_>>(),
        "candidates_total": report.candidates.len(),
        "failed": report.failed.iter().map(|(c, msg)| {
            serde_json::json!({"skill": c.skill, "error": msg})
        }).collect::<Vec<_>>(),
    });
    serde_json::to_writer_pretty(std::io::stdout(), &obj)?;
    println!();
    Ok(())
}
