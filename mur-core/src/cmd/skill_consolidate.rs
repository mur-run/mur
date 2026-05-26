//! CLI dispatcher for `mur skill consolidate` (M5b + M6c.1).

use std::io::IsTerminal;
use std::path::Path;

use anyhow::{Context, Result};

use crate::skill_consolidate::{ConsolidateMethod, ConsolidateOptions, run_consolidate};
use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::factory::get_vector_store;

pub async fn cmd_consolidate(
    home: &Path,
    dry_run: bool,
    apply: bool,
    method: ConsolidateMethod,
    llm_adjudicate: bool,
) -> Result<()> {
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

    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index_dir = home.join("lance");
    let store = get_vector_store(&cfg, &index_dir)
        .await
        .context("opening vector store")?;

    let opts = ConsolidateOptions {
        dry_run,
        apply: apply && !dry_run,
        method: method.clone(),
        llm_adjudicate,
    };
    let report = run_consolidate(home, &embed_config, &*store, &opts).await?;
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
            "  Duplicate: {} ≈ {} (sim={:.3}, keeper={}, source={})",
            d.a,
            d.b,
            d.similarity,
            d.keeper,
            serde_json::to_string(&d.source).unwrap_or_default(),
        );
    }
    for c in &report.contradictions {
        let adj = c
            .adjudication
            .as_ref()
            .map(|v| format!(" [adjudicated: {}]", v.as_str()))
            .unwrap_or_default();
        println!(
            "  Contradiction: {} vs {} on trigger '{}' — {}{adj}",
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
