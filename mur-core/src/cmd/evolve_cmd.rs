use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use crate::evolve;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

pub(crate) fn cmd_consolidate(dry_run: bool) -> Result<()> {
    let store = YamlStore::default_store()?;
    let mode = if dry_run { "dry run" } else { "live" };
    println!("🧹 Consolidating patterns ({})...\n", mode);

    let report = evolve::consolidate::consolidate(&store, dry_run)?;

    println!("Scanned: {} patterns", report.patterns_scanned);
    if report.duplicates_merged > 0 {
        println!("  🔗 Merged {} duplicates", report.duplicates_merged);
    }
    if report.contradictions_resolved > 0 {
        println!(
            "  ⚡ Resolved {} contradictions",
            report.contradictions_resolved
        );
    }
    if report.promotions > 0 {
        println!("  ⬆️  Promoted {} patterns (tier)", report.promotions);
    }
    if report.maturity_promotions + report.maturity_demotions > 0 {
        println!(
            "  🎯 Maturity: {} promotions, {} demotions",
            report.maturity_promotions, report.maturity_demotions
        );
    }
    if report.patterns_decayed > 0 {
        println!("  📉 Decayed {} patterns", report.patterns_decayed);
    }
    if report.patterns_archived > 0 {
        println!("  📦 Archived {} patterns", report.patterns_archived);
    }

    if !report.details.is_empty() {
        println!();
        for d in &report.details {
            println!("  {} [{}] {}", d.pattern_name, d.action, d.detail);
        }
    }

    if dry_run {
        println!("\n(dry run — no changes saved)");
    }

    Ok(())
}

pub(crate) fn cmd_evolve(dry_run: bool, _force: bool) -> Result<()> {
    use evolve::decay::{apply_decay_all, apply_decay_all_dry_run};
    use evolve::maturity::{apply_maturity_all, apply_maturity_all_dry_run};
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    if dry_run {
        println!("🔮 Evolve (dry run) — previewing changes...\n");
    } else {
        println!("🔮 Evolving patterns...\n");
    }

    // Phase 1: Decay
    let decay_report = if dry_run {
        apply_decay_all_dry_run(&store, now)?
    } else {
        apply_decay_all(&store, now)?
    };

    if !decay_report.details.is_empty() {
        println!("📉 Decay:");
        for d in &decay_report.details {
            let archived = if d.auto_archived { " → ARCHIVED" } else { "" };
            println!(
                "  {} — confidence {:.0}% → {:.0}%{}",
                d.name,
                d.old_confidence * 100.0,
                d.new_confidence * 100.0,
                archived,
            );
        }
        println!();
    }

    // Phase 2: Maturity
    let maturity_report = if dry_run {
        apply_maturity_all_dry_run(&store, now)?
    } else {
        apply_maturity_all(&store, now)?
    };

    if !maturity_report.details.is_empty() {
        println!("🎯 Maturity:");
        for d in &maturity_report.details {
            let arrow = if d.is_promotion { "⬆" } else { "⬇" };
            println!(
                "  {} {} — {:?} → {:?}",
                arrow, d.name, d.old_maturity, d.new_maturity,
            );
        }
        println!();
    }

    // Summary
    let mode = if dry_run { " (dry run)" } else { "" };
    println!("── Summary{} ──", mode);
    println!("  Scanned:        {}", decay_report.patterns_scanned);
    println!("  Decayed:        {}", decay_report.patterns_decayed);
    println!("  Auto-archived:  {}", decay_report.patterns_archived);
    println!("  Promotions:     {}", maturity_report.promotions);
    println!("  Demotions:      {}", maturity_report.demotions);

    // Show maturity distribution
    let patterns = store.list_all()?;
    let (mut draft, mut emerging, mut stable, mut canonical) = (0usize, 0, 0, 0);
    for p in &patterns {
        match p.maturity {
            Maturity::Draft => draft += 1,
            Maturity::Emerging => emerging += 1,
            Maturity::Stable => stable += 1,
            Maturity::Canonical => canonical += 1,
        }
    }
    println!(
        "  Maturity:       Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft, emerging, stable, canonical
    );

    Ok(())
}

pub(crate) fn cmd_evolve_compose(create: bool) -> Result<()> {
    use evolve::compose::suggest_workflows_with_patterns;
    use evolve::cooccurrence::CooccurrenceMatrix;

    let pattern_store = YamlStore::default_store()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("🔗 Workflow Composition from Co-occurrence\n");
    println!("  Tracked pairs: {}", matrix.pair_count());

    let suggestions = suggest_workflows_with_patterns(&matrix, 5, &patterns);

    if suggestions.is_empty() {
        println!("  No workflow composition suggestions yet.");
        println!("  (Need 3+ patterns co-occurring 5+ times)");
        return Ok(());
    }

    println!();
    for (i, s) in suggestions.iter().enumerate() {
        println!(
            "  {}. {} (score: {})",
            i + 1,
            s.suggested_name,
            s.cooccurrence_score,
        );
        println!("     Patterns: {}", s.patterns.join(", "));
        println!("     Trigger: {}", s.suggested_trigger);

        if create {
            if workflow_store.exists(&s.suggested_name) {
                println!(
                    "     -> Workflow '{}' already exists, skipping.",
                    s.suggested_name
                );
            } else {
                let wf = mur_common::workflow::Workflow {
                    base: KnowledgeBase {
                        name: s.suggested_name.clone(),
                        description: format!(
                            "Auto-suggested workflow from {} co-occurring patterns",
                            s.patterns.len()
                        ),
                        content: Content::Plain(format!(
                            "Combines patterns: {}",
                            s.patterns.join(", ")
                        )),
                        tags: crate::cmd::workflow::collect_tags_from_patterns(
                            &s.patterns,
                            &patterns,
                        ),
                        ..Default::default()
                    },
                    steps: vec![],
                    variables: vec![],
                    source_sessions: vec![],
                    trigger: s.suggested_trigger.clone(),
                    tools: vec![],
                    published_version: 0,
                    permission: Default::default(),
                };
                workflow_store.save(&wf)?;
                println!("     -> Created draft workflow: {}", s.suggested_name);
            }
        }
        println!();
    }

    Ok(())
}

pub(crate) fn cmd_evolve_cooccurrence(min_count: u32) -> Result<()> {
    use evolve::cooccurrence::CooccurrenceMatrix;

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("📊 Co-occurrence Matrix\n");
    println!("  Total tracked pairs: {}", matrix.pair_count());

    let mut pairs = matrix.all_pairs();
    pairs.sort_by(|a, b| b.1.cmp(&a.1));

    let filtered: Vec<_> = pairs
        .iter()
        .filter(|(_, count)| *count >= min_count)
        .collect();
    if filtered.is_empty() {
        println!("  No pairs with count >= {}", min_count);
        return Ok(());
    }

    println!("  Pairs with count >= {}:\n", min_count);
    for ((a, b), count) in &filtered {
        println!("    {} <-> {} : {}", a, b, count);
    }

    // Show clusters
    let clusters = matrix.find_clusters(min_count);
    if !clusters.is_empty() {
        println!("\n  Clusters (connected components):\n");
        for (i, c) in clusters.iter().enumerate() {
            println!(
                "    {}. {} (score: {})",
                i + 1,
                c.suggested_workflow_name,
                c.total_cooccurrences
            );
            println!("       Patterns: {}", c.pattern_names.join(", "));
        }
    }

    Ok(())
}
