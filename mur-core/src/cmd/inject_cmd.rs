use anyhow::Result;
use mur_common::pattern::*;

use crate::inject;
use crate::interactive;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

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

pub(crate) fn cmd_why(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;
    interactive::explain_why(&pattern, &store)
}
