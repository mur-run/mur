use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use crate::evolve;
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
