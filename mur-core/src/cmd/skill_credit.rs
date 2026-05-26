//! `mur skill credit <name>` CLI handler (M7c).

use std::path::Path;

use anyhow::Result;
use mur_common::skill::credit::{CreditEvidence, CreditKind};

use crate::cross_agent::credit::aggregate::{CreditView, build_credit_view};

pub fn cmd_credit(home: &Path, agent: &str, skill: &str, json: bool) -> Result<()> {
    let view = build_credit_view(home, agent, skill)?;
    if view.entries.is_empty() {
        eprintln!("no credit history for {skill}");
        std::process::exit(2);
    }
    if json {
        emit_json(&view)?;
    } else {
        emit_human(&view);
    }
    Ok(())
}

fn emit_human(view: &CreditView) {
    println!("Skill: {}", view.skill);
    println!();

    let authors: Vec<_> = view
        .entries
        .iter()
        .filter(|e| e.kind == CreditKind::Author)
        .collect();
    if !authors.is_empty() {
        println!("Author{}:", if authors.len() > 1 { "s" } else { "" });
        for e in &authors {
            println!(
                "  {:<8} {}  source: {}",
                e.agent,
                e.ts.to_rfc3339(),
                e.source
            );
        }
        println!();
    }

    let mutators: Vec<_> = view
        .entries
        .iter()
        .filter(|e| e.kind == CreditKind::Mutator)
        .collect();
    if !mutators.is_empty() {
        println!("Mutators ({}):", mutators.len());
        for e in &mutators {
            if let Some(CreditEvidence::Mutator {
                from_version,
                diff_summary,
            }) = &e.evidence
            {
                println!(
                    "  {:<8} {}  v{} → v{}  (\"{}\")",
                    e.agent,
                    e.ts.to_rfc3339(),
                    from_version,
                    e.skill_version,
                    diff_summary
                );
            } else {
                println!(
                    "  {:<8} {}  v{}",
                    e.agent,
                    e.ts.to_rfc3339(),
                    e.skill_version
                );
            }
        }
        println!();
    }

    let recomb: Vec<_> = view
        .entries
        .iter()
        .filter(|e| e.kind == CreditKind::Recombiner)
        .collect();
    if !recomb.is_empty() {
        println!("Recombiners ({}):", recomb.len());
        for e in &recomb {
            if let Some(CreditEvidence::Recombiner { role, child }) = &e.evidence {
                println!(
                    "  {:<8} {}  {} → {}",
                    e.agent,
                    e.ts.to_rfc3339(),
                    role,
                    child
                );
            }
        }
        println!();
    }

    let prop: Vec<_> = view
        .entries
        .iter()
        .filter(|e| e.kind == CreditKind::Propagator)
        .collect();
    if !prop.is_empty() {
        println!("Propagators ({}):", prop.len());
        for e in &prop {
            if let Some(CreditEvidence::Propagator {
                from_agent,
                fitness_at_install,
                samples_at_install,
            }) = &e.evidence
            {
                println!(
                    "  {:<8} {}  v{}  ← agent://{}  (fitness {:.2}, n={})",
                    e.agent,
                    e.ts.to_rfc3339(),
                    e.skill_version,
                    from_agent,
                    fitness_at_install,
                    samples_at_install
                );
            }
        }
        println!();
    }

    println!(
        "Lineage summary: {} author(s), {} mutator(s), {} recombiner(s), {} propagation(s).",
        authors.len(),
        mutators.len(),
        recomb.len(),
        prop.len()
    );
}

fn emit_json(view: &CreditView) -> Result<()> {
    serde_json::to_writer_pretty(
        std::io::stdout(),
        &serde_json::json!({
            "skill": view.skill,
            "entries": view.entries,
        }),
    )?;
    println!();
    Ok(())
}
