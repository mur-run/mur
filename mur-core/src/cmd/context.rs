use anyhow::Result;
use mur_common::pattern::*;

use crate::capture;
use crate::context_api;
use crate::inject;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

pub(crate) async fn cmd_context(
    query: Option<String>,
    compact: bool,
    write_file: bool,
    budget: usize,
    source: String,
    json_output: bool,
    scope_args: Vec<String>,
) -> Result<()> {
    crate::auth::heartbeat();
    use crate::retrieve::scoring::{
        ScopeContext, score_and_rank_hybrid_with_scope, score_and_rank_with_scope,
    };
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::lancedb::VectorStore;
    use std::collections::HashMap;

    // Auto-pull from device sync if configured
    if let Ok(config) = crate::store::config::load_config()
        && config.sync.auto
        && config.sync.method != "local"
        && let Err(e) =
            super::sync_cmd::device_sync(true, super::sync_cmd::DeviceSyncDirection::Pull)
    {
        eprintln!("  ⚠ Auto-pull failed: {}", e);
    }

    // Parse scope arguments (key=value pairs)
    let mut scope = context_api::ContextScope::default();
    for arg in &scope_args {
        if let Some((key, value)) = arg.split_once('=') {
            match key {
                "user" => scope.user = Some(value.to_string()),
                "project" => scope.project = Some(value.to_string()),
                "task" => scope.task = Some(value.to_string()),
                "platform" => scope.platform = Some(value.to_string()),
                _ => eprintln!("Warning: unknown scope key '{}'", key),
            }
        }
    }

    // Auto-detect project context from cwd
    let cwd = std::env::current_dir()?;

    // Auto-detect and generate starter patterns for new projects
    if !capture::starter::is_known_project(&cwd)? {
        let starter_store = YamlStore::default_store()?;
        let existing: std::collections::HashSet<String> =
            starter_store.list_names()?.into_iter().collect();
        let starters = capture::starter::generate_starter_patterns(&cwd, &existing)?;
        if !starters.is_empty() {
            let lang_name = capture::starter::detect_language_name(&cwd)
                .unwrap_or_else(|| "unknown".to_string());
            if !compact {
                eprintln!(
                    "New project detected: {} ({} starter patterns generated)",
                    lang_name,
                    starters.len()
                );
            }
            let generated_names: Vec<String> = starters.iter().map(|p| p.name.clone()).collect();
            let deps: Vec<String> = starters
                .iter()
                .flat_map(|p| {
                    p.tags
                        .topics
                        .iter()
                        .filter(|t| t.as_str() != "starter")
                        .cloned()
                })
                .collect();
            for p in &starters {
                starter_store.save(p)?;
            }
            capture::starter::mark_project_known(
                &cwd,
                capture::starter::ProjectInfo {
                    path: cwd.to_string_lossy().to_string(),
                    language: capture::starter::detect_language(&cwd)
                        .unwrap_or(capture::starter::Language::Rust),
                    deps,
                    generated_at: chrono::Utc::now().to_rfc3339(),
                    patterns_generated: generated_names,
                },
            )?;
        } else {
            // No patterns generated but still mark as known to avoid re-scanning
            if let Some(lang) = capture::starter::detect_language(&cwd) {
                capture::starter::mark_project_known(
                    &cwd,
                    capture::starter::ProjectInfo {
                        path: cwd.to_string_lossy().to_string(),
                        language: lang,
                        deps: vec![],
                        generated_at: chrono::Utc::now().to_rfc3339(),
                        patterns_generated: vec![],
                    },
                )?;
            }
        }
    }

    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Build query from provided or auto-detected context
    let effective_query = match query {
        Some(q) => q,
        None => {
            // Auto-detect: project name + recent git context
            let mut parts = vec![project_name.clone()];

            // Try to get git remote for additional context
            if let Ok(output) = std::process::Command::new("git")
                .args(["remote", "get-url", "origin"])
                .current_dir(&cwd)
                .output()
                && output.status.success()
            {
                let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
                // Extract repo name from URL
                if let Some(name) = remote.rsplit('/').next() {
                    let name = name.trim_end_matches(".git");
                    if name != project_name {
                        parts.push(name.to_string());
                    }
                }
            }

            // Try to get recent file context
            if let Ok(output) = std::process::Command::new("git")
                .args(["diff", "--name-only", "HEAD~3..HEAD"])
                .current_dir(&cwd)
                .output()
                && output.status.success()
            {
                let files = String::from_utf8_lossy(&output.stdout);
                for file in files.lines().take(5) {
                    // Extract keywords from file paths
                    let stem = std::path::Path::new(file)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    if !stem.is_empty() && stem.len() > 2 {
                        parts.push(stem.to_string());
                    }
                }
            }

            parts.join(" ")
        }
    };

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // Build scope context for accurate preference/procedure scoring boosts.
    // `source` tells us the calling tool (e.g. "claude-code"), which maps
    // to `origin.platform` for preference matching.
    let score_scope = ScopeContext {
        user: scope.user.clone(),
        platform: scope.platform.clone().or_else(|| Some(source.clone())),
        task: scope.task.clone(),
    };

    // Try hybrid search if LanceDB index exists
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    let results = if index_path.exists() {
        let cfg = crate::store::config::load_config()?;
        let config = EmbeddingConfig::from_config(&cfg);
        match embed(&effective_query, &config).await {
            Ok(query_embedding) => {
                let vector_store =
                    VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
                let vector_results = vector_store.search(&query_embedding, 20, None).await?;
                let vector_scores: HashMap<String, f64> = vector_results
                    .into_iter()
                    .map(|r| (r.name, r.similarity as f64))
                    .collect();
                score_and_rank_hybrid_with_scope(
                    &effective_query,
                    patterns,
                    &vector_scores,
                    Some(&score_scope),
                )
            }
            Err(_) => score_and_rank_with_scope(&effective_query, patterns, Some(&score_scope)),
        }
    } else {
        score_and_rank_with_scope(&effective_query, patterns, Some(&score_scope))
    };

    let mut injected_patterns: Vec<Pattern> = Vec::new();
    for sp in results {
        let mut p = sp.pattern;
        if p.lifecycle.status == LifecycleStatus::Archived {
            continue;
        }
        let now = chrono::Utc::now();
        p.decay.last_active = Some(now);
        p.evidence.injection_count += 1;
        p.lifecycle.last_injected = Some(now);
        p.updated_at = now;
        // Track project usage for cross-project learning
        if !project_name.is_empty() && !p.applies.projects.contains(&project_name) {
            p.applies.projects.push(project_name.clone());
        }
        let _ = yaml_store.save(&p);
        injected_patterns.push(p);
    }

    let token_budget = if compact { 800 } else { budget };

    // If JSON output requested, build ContextResponse from the already-scored
    // injected_patterns (same hybrid pipeline as text path, same project scope).
    if json_output {
        use context_api::{ContextResponse, ScoredPatternResponse};
        let response_patterns: Vec<ScoredPatternResponse> = injected_patterns
            .iter()
            .map(|p| {
                let kind_str = match p.effective_kind() {
                    mur_common::pattern::PatternKind::Technical => "technical",
                    mur_common::pattern::PatternKind::Preference => "preference",
                    mur_common::pattern::PatternKind::Fact => "fact",
                    mur_common::pattern::PatternKind::Procedure => "procedure",
                    mur_common::pattern::PatternKind::Behavioral => "behavioral",
                };
                ScoredPatternResponse {
                    name: p.name.clone(),
                    description: p.description.clone(),
                    score: p.importance, // best proxy available after injection update
                    kind: kind_str.to_string(),
                }
            })
            .collect();
        let formatted = inject::hook::format_unified_injection_with_store(
            &injected_patterns,
            &workflows,
            token_budget,
            Some(&yaml_store),
        );
        let resp = ContextResponse {
            tokens_used: formatted.len() / 4,
            injection_ids: injected_patterns.iter().map(|p| p.name.clone()).collect(),
            patterns: response_patterns,
            formatted,
        };
        let json = serde_json::to_string_pretty(&resp)?;
        println!("{json}");
        return Ok(());
    }

    let output = inject::hook::format_unified_injection_with_store(
        &injected_patterns,
        &workflows,
        token_budget,
        Some(&yaml_store),
    );

    if !output.is_empty() {
        inject::hook::record_injection(&effective_query, &project_name, &injected_patterns);
        inject::hook::record_cooccurrence_for_injection(&injected_patterns);

        if write_file {
            // Write to ~/.mur/context.md for file-based tools
            let context_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("context.md");
            let file_content = format!(
                "# MUR Context (auto-generated)\n# Query: {}\n# Updated: {}\n\n{}\n",
                effective_query,
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                output
            );
            std::fs::write(&context_path, file_content)?;
            eprintln!("📝 Context written to {}", context_path.display());
        } else {
            print!("{}", output);
        }
    }

    Ok(())
}
