use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use crate::evolve;
use crate::inject;
use crate::interactive;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

pub(crate) fn cmd_stats() -> Result<()> {
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let total = patterns.len();
    let mut session_count = 0;
    let mut project_count = 0;
    let mut core_count = 0;
    let mut active_count = 0;
    let mut deprecated_count = 0;
    let mut archived_count = 0;
    let mut draft_count = 0;
    let mut emerging_count = 0;
    let mut stable_count = 0;
    let mut canonical_count = 0;
    let mut total_importance = 0.0;
    let mut total_effectiveness = 0.0;
    let mut tracked_count = 0u64;
    let mut total_injections = 0u64;

    for p in &patterns {
        match p.tier {
            Tier::Session => session_count += 1,
            Tier::Project => project_count += 1,
            Tier::Core => core_count += 1,
        }
        match p.lifecycle.status {
            LifecycleStatus::Active => active_count += 1,
            LifecycleStatus::Deprecated => deprecated_count += 1,
            LifecycleStatus::Archived => archived_count += 1,
        }
        match p.maturity {
            Maturity::Draft => draft_count += 1,
            Maturity::Emerging => emerging_count += 1,
            Maturity::Stable => stable_count += 1,
            Maturity::Canonical => canonical_count += 1,
        }
        total_importance += p.importance;
        total_injections += p.evidence.injection_count;
        if p.evidence.injection_count > 0 {
            tracked_count += 1;
            total_effectiveness += p.evidence.effectiveness();
        }
    }

    let avg_importance = if total > 0 {
        total_importance / total as f64
    } else {
        0.0
    };
    let avg_effectiveness = if tracked_count > 0 {
        total_effectiveness / tracked_count as f64
    } else {
        0.0
    };

    println!("📊 MUR Core v2 Statistics");
    println!("─────────────────────────");
    println!("Total patterns:     {}", total);
    println!();
    println!("By tier:");
    println!("  📝 Session:       {}", session_count);
    println!("  📁 Project:       {}", project_count);
    println!("  ⭐ Core:          {}", core_count);
    println!();
    println!("By status:");
    println!("  ✅ Active:        {}", active_count);
    println!("  ⚠️  Deprecated:    {}", deprecated_count);
    println!("  📦 Archived:      {}", archived_count);
    println!();
    println!(
        "By maturity:        Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft_count, emerging_count, stable_count, canonical_count
    );
    println!();
    println!("Avg importance:     {:.0}%", avg_importance * 100.0);
    println!("Total injections:   {}", total_injections);
    println!(
        "Tracked patterns:   {} / {} ({:.0}%)",
        tracked_count,
        total,
        if total > 0 {
            tracked_count as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );
    println!("Avg effectiveness:  {:.0}%", avg_effectiveness * 100.0);

    Ok(())
}

pub(crate) fn cmd_gc(auto: bool) -> Result<()> {
    use evolve::decay::apply_decay_all;
    use evolve::lifecycle::{LifecycleAction, apply_lifecycle_action, evaluate_lifecycle};
    use evolve::maturity::apply_maturity_all;
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    // Run decay + maturity before lifecycle evaluation
    let decay_report = apply_decay_all(&store, now)?;
    if decay_report.patterns_decayed > 0 {
        println!(
            "📉 Decayed {} patterns ({} auto-archived).",
            decay_report.patterns_decayed, decay_report.patterns_archived
        );
    }

    let maturity_report = apply_maturity_all(&store, now)?;
    if maturity_report.promotions + maturity_report.demotions > 0 {
        println!(
            "🎯 Maturity: {} promotions, {} demotions.",
            maturity_report.promotions, maturity_report.demotions
        );
    }

    let patterns = store.list_all()?;

    // First pass: apply lifecycle evaluations (deprecate/archive)
    let mut lifecycle_changes = 0;
    for mut p in patterns.clone() {
        let action = evaluate_lifecycle(&p);
        if action != LifecycleAction::None {
            let desc = match &action {
                LifecycleAction::Promote(tier) => format!("promoted to {:?}", tier),
                LifecycleAction::Deprecate => "deprecated".into(),
                LifecycleAction::Archive => "archived".into(),
                LifecycleAction::None => unreachable!(),
            };
            println!("  🔄 {} — {}", p.name, desc);
            apply_lifecycle_action(&mut p, &action);
            store.save(&p)?;
            lifecycle_changes += 1;
        }
    }
    if lifecycle_changes > 0 {
        println!("Applied {} lifecycle changes.\n", lifecycle_changes);
    }

    // Link discovery pass (pattern↔pattern and pattern↔workflow)
    {
        use evolve::linker::{
            LinkType, apply_workflow_links, discover_links, discover_workflow_links,
        };
        let all = store.list_all()?;
        let wf_store = WorkflowYamlStore::default_store()?;
        let workflows = wf_store.list_all()?;

        // Phase 1: Collect all pattern↔pattern link suggestions (read-only)
        let mut pairs: Vec<(String, String, LinkType)> = Vec::new();
        for pattern in &all {
            for s in discover_links(pattern, &all) {
                pairs.push((pattern.name.clone(), s.target_name, s.link_type));
            }
        }

        // Phase 1b: Collect pattern↔workflow link suggestions
        let mut wf_links: Vec<(String, Vec<String>)> = Vec::new();
        if !workflows.is_empty() {
            for pattern in &all {
                let suggestions = discover_workflow_links(pattern, &workflows);
                if !suggestions.is_empty() {
                    let wf_names: Vec<String> = suggestions
                        .iter()
                        .map(|s| s.workflow_name.clone())
                        .collect();
                    wf_links.push((pattern.name.clone(), wf_names));
                }
            }
        }

        let has_pattern_links = !pairs.is_empty();
        let has_workflow_links = !wf_links.is_empty();

        if has_pattern_links || has_workflow_links {
            // Phase 2: Apply pattern↔pattern links
            let mut by_name: std::collections::HashMap<String, Pattern> =
                all.into_iter().map(|p| (p.name.clone(), p)).collect();
            let mut changed = std::collections::HashSet::new();
            let mut link_count = 0usize;

            for (source, target, link_type) in &pairs {
                match link_type {
                    LinkType::Related => {
                        if let Some(p) = by_name.get_mut(source)
                            && !p.links.related.contains(target)
                        {
                            p.links.related.push(target.clone());
                            changed.insert(source.clone());
                            link_count += 1;
                        }
                        if let Some(p) = by_name.get_mut(target)
                            && !p.links.related.contains(source)
                        {
                            p.links.related.push(source.clone());
                            changed.insert(target.clone());
                            link_count += 1;
                        }
                    }
                    LinkType::Supersedes => {
                        if let Some(p) = by_name.get_mut(source)
                            && !p.links.supersedes.contains(target)
                        {
                            p.links.supersedes.push(target.clone());
                            changed.insert(source.clone());
                            link_count += 1;
                        }
                    }
                }
            }

            // Phase 2b: Apply pattern↔workflow links
            for (pattern_name, wf_names) in &wf_links {
                if let Some(p) = by_name.get_mut(pattern_name) {
                    let suggestions: Vec<evolve::linker::WorkflowLinkSuggestion> = wf_names
                        .iter()
                        .map(|name| evolve::linker::WorkflowLinkSuggestion {
                            workflow_name: name.clone(),
                            score: 0.0, // score not needed for apply
                        })
                        .collect();
                    let before = p.links.workflows.len();
                    apply_workflow_links(p, &suggestions);
                    let added = p.links.workflows.len() - before;
                    if added > 0 {
                        changed.insert(pattern_name.clone());
                        link_count += added;
                    }
                }
            }

            // Phase 3: Save changed patterns
            for name in &changed {
                if let Some(p) = by_name.get(name) {
                    store.save(p)?;
                }
            }

            if link_count > 0 {
                println!("🔗 Discovered {} new links.\n", link_count);
            }
        }
    }

    // Second pass: find low-quality candidates for archival
    let patterns = store.list_all()?; // reload after changes
    let candidates: Vec<&Pattern> = patterns
        .iter()
        .filter(|p| {
            if p.lifecycle.pinned || p.lifecycle.status != LifecycleStatus::Active {
                return false;
            }
            // Low confidence
            if p.confidence < 0.5 {
                return true;
            }
            // Low effectiveness with enough data
            let total = p.evidence.success_signals + p.evidence.override_signals;
            if total >= 5 && p.evidence.effectiveness() < 0.2 {
                return true;
            }
            false
        })
        .collect();

    if candidates.is_empty() {
        println!("✨ No patterns need cleanup.");
        return Ok(());
    }

    println!("🧹 Found {} patterns for cleanup:\n", candidates.len());
    for p in &candidates {
        let reason = if p.confidence < 0.5 {
            format!("low confidence ({:.0}%)", p.confidence * 100.0)
        } else {
            format!(
                "low effectiveness ({:.0}%)",
                p.evidence.effectiveness() * 100.0
            )
        };
        println!("  {} — {}", p.name, reason);
    }

    if auto {
        println!();
        let mut archived = 0;
        for p in &candidates {
            if store.archive(&p.name)? {
                archived += 1;
            }
        }
        println!("📦 Archived {} patterns.", archived);
    } else {
        println!("\nRun `mur gc --auto` to archive these patterns.");
    }

    // Show maturity distribution
    let all = store.list_all()?;
    let (mut draft, mut emerging, mut stable, mut canonical) = (0usize, 0, 0, 0);
    for p in &all {
        match p.maturity {
            Maturity::Draft => draft += 1,
            Maturity::Emerging => emerging += 1,
            Maturity::Stable => stable += 1,
            Maturity::Canonical => canonical += 1,
        }
    }
    println!(
        "\nMaturity: Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft, emerging, stable, canonical
    );

    Ok(())
}

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

pub(crate) fn cmd_analyze(dry_run: bool) -> Result<()> {
    use crate::capture::style::{analyze_style, format_as_pattern_content};

    let cwd = std::env::current_dir()?;
    let language = crate::capture::starter::detect_language_name(&cwd);

    let language = match language {
        Some(lang) => lang,
        None => {
            println!("Could not detect project language. Run from a project directory.");
            return Ok(());
        }
    };

    println!("🔍 Analyzing {} project...\n", language);

    let analysis = analyze_style(&cwd, &language);

    if analysis.files_scanned == 0 {
        println!("  No source files found to analyze.");
        return Ok(());
    }

    println!("  Files scanned:     {}", analysis.files_scanned);
    println!("  Naming convention: {}", analysis.naming);
    println!("  Indentation:       {}", analysis.indentation);
    println!("  Max line length:   {} chars", analysis.max_line_length);
    println!("  Import ordering:   {}", analysis.import_ordering);

    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let pattern_name = format!("{}-code-style", project_name);
    let content = format_as_pattern_content(&analysis);

    if dry_run {
        println!("\n  Would create pattern: {}", pattern_name);
        println!("  Content:\n    {}", content.replace('\n', "\n    "));
        println!("\n(dry run — no changes saved)");
    } else {
        let store = YamlStore::default_store()?;

        let pattern = Pattern {
            base: KnowledgeBase {
                schema: SCHEMA_VERSION,
                name: pattern_name.clone(),
                description: format!("Code style conventions for {} ({})", project_name, language),
                content: Content::Plain(content),
                tier: Tier::Project,
                importance: 0.6,
                confidence: 0.8,
                tags: Tags {
                    languages: vec![language.to_lowercase()],
                    topics: vec!["code-style".into(), "conventions".into()],
                    extra: Default::default(),
                },
                applies: Applies {
                    projects: vec![project_name.clone()],
                    ..Default::default()
                },
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: None,
            attachments: vec![],
        };

        if store.exists(&pattern_name) {
            // Update existing
            let mut existing = store.get(&pattern_name)?;
            existing.content = pattern.content.clone();
            existing.updated_at = chrono::Utc::now();
            store.save(&existing)?;
            println!("\n  Updated pattern: {}", pattern_name);
        } else {
            store.save(&pattern)?;
            println!("\n  Created pattern: {}", pattern_name);
        }
    }

    Ok(())
}

pub(crate) fn cmd_import(files: Option<Vec<String>>, dry_run: bool) -> Result<()> {
    use crate::capture::import;

    let cwd = std::env::current_dir()?;

    let paths: Vec<std::path::PathBuf> = if let Some(files) = files {
        files.into_iter().map(std::path::PathBuf::from).collect()
    } else {
        let detected = import::detect_files(&cwd);
        if detected.is_empty() {
            println!("No AI tool config files found in current directory.");
            println!(
                "Supported: {}",
                [
                    ".cursorrules",
                    ".windsurfrules",
                    ".clinerules",
                    "CLAUDE.md",
                    "AGENTS.md",
                    ".github/copilot-instructions.md"
                ]
                .join(", ")
            );
            return Ok(());
        }
        detected
    };

    let store = YamlStore::default_store()?;
    let existing: std::collections::HashSet<String> = store.list_names()?.into_iter().collect();

    let mut all_candidates = Vec::new();
    for path in &paths {
        match import::extract_from_file(path) {
            Ok(candidates) => {
                let filename = path.file_name().unwrap_or_default().to_string_lossy();
                println!("  Found: {} ({} rules)", filename, candidates.len());
                all_candidates.extend(candidates);
            }
            Err(e) => {
                eprintln!("  Warning: failed to read {}: {}", path.display(), e);
            }
        }
    }

    if all_candidates.is_empty() {
        println!("No importable rules found.");
        return Ok(());
    }

    let patterns = import::candidates_to_patterns(all_candidates, &existing);

    if patterns.is_empty() {
        println!("All rules already exist as patterns. Nothing to import.");
        return Ok(());
    }

    if dry_run {
        println!();
        println!("Would import {} patterns:", patterns.len());
        for p in &patterns {
            println!(
                "  - {} ({:?}) [{}]",
                p.name,
                p.effective_kind(),
                p.origin
                    .as_ref()
                    .map(|o| o.platform.as_deref().unwrap_or(""))
                    .unwrap_or("")
            );
        }
        return Ok(());
    }

    let count = patterns.len();
    for pattern in &patterns {
        store.save(pattern)?;
    }
    println!("Imported {} patterns (Project tier)", count);

    Ok(())
}

pub(crate) async fn cmd_reindex() -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::lancedb::VectorStore;

    let pattern_store = YamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    if patterns.is_empty() && workflows.is_empty() {
        println!("No patterns or workflows to index.");
        return Ok(());
    }

    let cfg = crate::store::config::load_config()?;
    let config = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    println!(
        "🔄 Reindexing {} patterns + {} workflows using {} ({})...",
        patterns.len(),
        workflows.len(),
        config.model,
        match &config.provider {
            crate::store::embedding::EmbeddingProvider::Ollama { base_url } => base_url.clone(),
            crate::store::embedding::EmbeddingProvider::OpenAI { .. } => "OpenAI".into(),
        }
    );

    let mut indexed_patterns = Vec::new();
    let mut indexed_workflows = Vec::new();
    let mut errors = 0;
    let total = patterns.len() + workflows.len();

    for (i, pattern) in patterns.iter().enumerate() {
        let mut text = format!(
            "{}: {}\n{}",
            pattern.name,
            pattern.description,
            pattern.content.as_text()
        );
        // Include attachment descriptions in embedding text for better search
        for att in &pattern.attachments {
            if !att.description.is_empty() {
                text.push_str("\n\n");
                text.push_str(&att.description);
            }
        }
        match embed(&text, &config).await {
            Ok(embedding) => {
                indexed_patterns.push((pattern.clone(), embedding));
                if (i + 1) % 10 == 0 {
                    println!("  {}/{} embedded...", i + 1, total);
                }
            }
            Err(e) => {
                eprintln!("  ⚠️  {} — {}", pattern.name, e);
                errors += 1;
            }
        }
    }

    for (i, workflow) in workflows.iter().enumerate() {
        let text = format!(
            "{}: {}\n{}",
            workflow.name,
            workflow.description,
            workflow.content.as_text()
        );
        match embed(&text, &config).await {
            Ok(embedding) => {
                indexed_workflows.push((workflow.clone(), embedding));
                let idx = patterns.len() + i + 1;
                if idx % 10 == 0 {
                    println!("  {}/{} embedded...", idx, total);
                }
            }
            Err(e) => {
                eprintln!("  ⚠️  {} — {}", workflow.name, e);
                errors += 1;
            }
        }
    }

    let vector_store = VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
    vector_store
        .build_unified_index(&indexed_patterns, &indexed_workflows)
        .await?;

    println!(
        "✅ Indexed {} patterns + {} workflows ({} errors). Index: {}",
        indexed_patterns.len(),
        indexed_workflows.len(),
        errors,
        index_path.display()
    );

    Ok(())
}

pub(crate) async fn cmd_inject(query: &str) -> Result<()> {
    use crate::retrieve::gate::{GateDecision, evaluate_query};
    use crate::retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::lancedb::VectorStore;
    use inject::hook::{HookTrigger, detect_trigger};
    use std::collections::HashMap;

    // Detect trigger type
    let trigger = detect_trigger(query);
    match &trigger {
        HookTrigger::OnError => {
            eprintln!("# Trigger: OnError — searching for error-related patterns")
        }
        HookTrigger::OnRetry => eprintln!("# Trigger: OnRetry — searching for previous solutions"),
        _ => {}
    }

    if let GateDecision::Skip(reason) = evaluate_query(query) {
        eprintln!("# No patterns (gate: {})", reason);
        return Ok(());
    }

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;

    // Also load workflows
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // Try hybrid search if LanceDB index exists
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    let results = if index_path.exists() {
        // Try vector search
        let cfg = crate::store::config::load_config()?;
        let config = EmbeddingConfig::from_config(&cfg);
        match embed(query, &config).await {
            Ok(query_embedding) => {
                let vector_store =
                    VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
                let vector_results = vector_store.search(&query_embedding, 20, None).await?;
                let vector_scores: HashMap<String, f64> = vector_results
                    .into_iter()
                    .map(|r| (r.name, r.similarity as f64))
                    .collect();
                score_and_rank_hybrid(query, patterns, &vector_scores)
            }
            Err(_) => {
                // Embedding failed (Ollama not running?), fall back to keyword
                eprintln!("# Falling back to keyword search (embedding unavailable)");
                score_and_rank(query, patterns)
            }
        }
    } else {
        score_and_rank(query, patterns)
    };

    // Filter out archived patterns, touch timestamps for injected ones
    let project_name = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();
    let mut injected_patterns: Vec<Pattern> = Vec::new();
    for sp in results {
        let mut p = sp.pattern;
        if p.lifecycle.status == LifecycleStatus::Archived {
            continue;
        }
        // Touch timestamps on injection
        let now = chrono::Utc::now();
        p.decay.last_active = Some(now);
        p.evidence.injection_count += 1;
        p.lifecycle.last_injected = Some(now);
        p.updated_at = now;
        // Track project usage for cross-project learning
        if !project_name.is_empty() && !p.applies.projects.contains(&project_name) {
            p.applies.projects.push(project_name.clone());
        }
        // Save touched pattern (best-effort, don't fail injection on save error)
        let _ = yaml_store.save(&p);
        injected_patterns.push(p);
    }

    let output = inject::hook::format_unified_injection_with_store(
        &injected_patterns,
        &workflows,
        2000,
        Some(&yaml_store),
    );

    if output.is_empty() {
        eprintln!("# No relevant patterns found");
    } else {
        inject::hook::record_injection(query, &project_name, &injected_patterns);

        // Record co-occurrence for pattern↔workflow intelligence
        inject::hook::record_cooccurrence_for_injection(&injected_patterns);

        print!("{}", output);
    }

    Ok(())
}

pub(crate) fn cmd_exchange_import(file: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let path = std::path::Path::new(file);
    match crate::store::exchange::import_mkef_file(path, &store)? {
        Some(name) => println!("✅ Imported pattern: {}", name),
        None => println!("⏭️  Pattern already exists, skipped"),
    }
    Ok(())
}

pub(crate) fn cmd_exchange_import_all() -> Result<()> {
    let store = YamlStore::default_store()?;
    let exchange_dir = crate::store::exchange::default_exchange_dir();
    let imported = crate::store::exchange::import_mkef_dir(&exchange_dir, &store)?;
    if imported.is_empty() {
        println!("No new patterns to import from {}", exchange_dir.display());
    } else {
        println!("✅ Imported {} patterns:", imported.len());
        for name in &imported {
            println!("  - {}", name);
        }
    }
    Ok(())
}

pub(crate) fn cmd_exchange_export(name: &str, dir: Option<String>) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;
    let exchange_dir = dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::store::exchange::default_exchange_dir);
    let path = crate::store::exchange::export_mkef(&pattern, &exchange_dir)?;
    println!("✅ Exported to {}", path.display());
    Ok(())
}

pub(crate) async fn cmd_serve(port: u16, open: bool, readonly: bool) -> Result<()> {
    let mur_dir = dirs::home_dir().expect("no home dir").join(".mur");

    let (events_tx, _) = tokio::sync::broadcast::channel(64);
    let state = crate::server::AppState {
        patterns_dir: mur_dir.join("patterns"),
        workflows_dir: mur_dir.join("workflows"),
        index_dir: mur_dir.join("index"),
        config: crate::server::ServerConfig { readonly },
        events_tx,
    };

    let open_url = if open {
        Some(format!("http://localhost:{}", port))
    } else {
        None
    };

    crate::server::run_server(state, port, open_url).await
}

pub(crate) fn cmd_why(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;
    interactive::explain_why(&pattern, &store)
}

pub(crate) async fn cmd_login() -> Result<()> {
    if let Some(_tokens) = crate::auth::load_tokens() {
        println!("Already logged in. Run `mur logout` first to re-authenticate.");
        return Ok(());
    }

    println!("Logging in to mur community...");
    let client = reqwest::Client::new();
    let tokens = crate::auth::device_code_flow(&client).await?;
    crate::auth::save_tokens(&tokens)?;
    println!();
    println!("  ✅ Logged in successfully! Token stored in ~/.mur/auth.json");
    Ok(())
}

pub(crate) fn cmd_logout() -> Result<()> {
    crate::auth::clear_tokens()?;
    println!("Logged out. Auth tokens removed.");
    Ok(())
}
