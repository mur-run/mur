//! Unified pattern + sources search handler. Extracted from `main.rs`.

#[cfg(feature = "sources")]
pub async fn cmd_search_unified(
    query: String,
    source: Vec<String>,
    result_type: String,
    only_sources: bool,
    only_patterns: bool,
    limit: usize,
    json: bool,
) -> anyhow::Result<()> {
    use crate::{retrieve, sources, store};
    use anyhow::Context;

    let want_patterns =
        !only_sources && (only_patterns || result_type == "patterns" || result_type == "all");
    let want_sources =
        !only_patterns && (only_sources || result_type == "sources" || result_type == "all");

    // SOURCES SIDE
    let mut source_hits: Vec<retrieve::UnifiedHit> = Vec::new();
    if want_sources {
        let cfg = store::config::load_config()?;
        let emb_cfg = store::embedding::EmbeddingConfig::from_config(&cfg);
        let index_path = dirs::home_dir()
            .context("no home dir")?
            .join(".mur")
            .join("index");
        let vector_store = store::vector::factory::get_vector_store(&cfg, &index_path).await?;
        let tantivy = sources::tantivy::TantivyIndex::open_or_create(
            &dirs::home_dir().context("no home dir")?.join(".mur"),
        )?;
        let source_weights: std::collections::HashMap<String, f32> = {
            let store = sources::instance::SourceInstanceStore::default_store()?;
            store
                .list()?
                .into_iter()
                .map(|i| (i.id, i.weight))
                .collect()
        };
        let filter = store::vector::SearchFilter {
            source_ids: if source.is_empty() {
                None
            } else {
                Some(source)
            },
            since: None,
        };
        source_hits = retrieve::retrieve_unified(
            &query,
            vector_store,
            &tantivy,
            &emb_cfg,
            &source_weights,
            &filter,
            0,
            limit,
            0.35,
        )
        .await?;
    }

    // PATTERNS SIDE — keyword + scoring fallback (full unification deferred to §8.1).
    let pattern_results: Vec<(String, f64)> = if want_patterns {
        existing_pattern_search_names(&query)
            .await
            .unwrap_or_default()
    } else {
        vec![]
    };

    if json {
        let j = serde_json::json!({
            "patterns": pattern_results.iter().map(|(name, score)| serde_json::json!({
                "name": name,
                "score": score,
            })).collect::<Vec<_>>(),
            "sources": source_hits.iter().map(|u| serde_json::json!({
                "chunk_id": u.hit.chunk_id,
                "source_id": u.hit.source_id,
                "external_id": u.hit.external_id,
                "score": u.hit.score,
                "heading_path": u.hit.heading_path,
                "updated_at": u.hit.updated_at.to_rfc3339(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }

    if !pattern_results.is_empty() {
        println!("## Patterns ({})", pattern_results.len());
        for (name, score) in &pattern_results {
            println!("  [{:.3}] {}", score, name);
        }
    }
    if !source_hits.is_empty() {
        if !pattern_results.is_empty() {
            println!();
        }
        println!("## Notes ({})", source_hits.len());
        for u in &source_hits {
            let hp = if u.hit.heading_path.is_empty() {
                String::new()
            } else {
                format!(" § {}", u.hit.heading_path.join(" / "))
            };
            println!(
                "  [{:.3}] {} / {}{}",
                u.hit.score, u.hit.source_id, u.hit.external_id, hp
            );
        }
    }
    if pattern_results.is_empty() && source_hits.is_empty() {
        println!("(no hits)");
    }
    Ok(())
}

/// Thin adapter that reuses the existing pattern scoring pipeline and returns
/// `(pattern_name, score)` pairs. Falls back to an empty vec on error so the
/// source-retrieval section still renders.
#[cfg(feature = "sources")]
async fn existing_pattern_search_names(query: &str) -> anyhow::Result<Vec<(String, f64)>> {
    use crate::{retrieve::scoring::score_and_rank, store::yaml::YamlStore};

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;
    let scored = score_and_rank(query, patterns);
    let results = scored
        .into_iter()
        .map(|sp| (sp.pattern.name.clone(), sp.score))
        .collect();
    Ok(results)
}

#[cfg(not(feature = "sources"))]
pub async fn cmd_search_unified(
    query: String,
    _source: Vec<String>,
    _result_type: String,
    _only_sources: bool,
    _only_patterns: bool,
    _limit: usize,
    _json: bool,
) -> anyhow::Result<()> {
    crate::cmd::pattern::cmd_search(&query)?;
    Ok(())
}
