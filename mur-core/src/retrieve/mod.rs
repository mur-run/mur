pub mod gate;
pub mod scoring;

// ---------- P1.3: sources retrieve ----------

use std::sync::Arc;

use crate::sources::tantivy::TantivyIndex;
use crate::store::embedding::{EmbeddingConfig, embed};
use crate::store::vector::{Hit, SearchFilter, VectorStore};

/// Unified hit. Tagged with kind so the caller / formatter can split sections.
#[derive(Debug, Clone)]
pub struct UnifiedHit {
    pub kind: HitKind,
    pub hit: Hit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    Pattern,
    Source,
}

/// Retrieve across sources. Merges vector + tantivy BM25 results, applies
/// per-source weight and freshness via `score_sources()`, truncates to
/// `max_notes`, and filters by absolute `floor`.
///
/// The pattern side is NOT included here in P1.3 — callers invoke the
/// existing pattern retrieve pipeline separately and concatenate. See
/// design spec §8.1.
pub async fn retrieve_unified(
    query: &str,
    vector_store: Arc<dyn VectorStore>,
    tantivy: &TantivyIndex,
    embedding_cfg: &EmbeddingConfig,
    source_weights: &std::collections::HashMap<String, f32>,
    filter: &SearchFilter,
    _max_patterns: usize,
    max_notes: usize,
    floor: f32,
) -> anyhow::Result<Vec<UnifiedHit>> {
    let qvec = embed(query, embedding_cfg).await?;

    // Sources: combine vector hits + BM25 hits.
    let vec_hits = vector_store.search(&qvec, max_notes * 4, filter).await?;
    let bm25_hits = tantivy
        .search(query, max_notes * 4, filter.source_ids.as_deref())
        .unwrap_or_default();

    // Build a lookup for BM25 scores by chunk_id.
    let mut bm25_by_id: std::collections::HashMap<String, f32> =
        std::collections::HashMap::new();
    for h in &bm25_hits {
        bm25_by_id.insert(h.chunk_id.clone(), h.score);
    }

    // Merge: 0.7 * vec + 0.3 * bm25_sigmoid when both are present; single-axis otherwise.
    let mut merged: Vec<Hit> = vec_hits
        .into_iter()
        .map(|mut h| {
            let bm = bm25_by_id.remove(&h.chunk_id).unwrap_or(0.0);
            let bm_norm = (bm / (bm + 1.0)).clamp(0.0, 1.0);
            h.score = 0.7 * h.score + 0.3 * bm_norm;
            h
        })
        .collect();

    // BM25-only hits (no vector hit in top-k) are admitted with score 0.3*bm_norm.
    for h in bm25_hits {
        if merged.iter().any(|m| m.chunk_id == h.chunk_id) {
            continue;
        }
        let bm_norm = (h.score / (h.score + 1.0)).clamp(0.0, 1.0);
        merged.push(Hit {
            chunk_id: h.chunk_id,
            source_id: h.source_id,
            external_id: h.external_id,
            score: 0.3 * bm_norm,
            text: String::new(),
            heading_path: vec![],
            updated_at: chrono::Utc::now(),
        });
    }

    // Apply source-specific scoring.
    let scored = crate::retrieve::scoring::score_sources(merged, source_weights);

    // Floor + cap.
    let out: Vec<UnifiedHit> = scored
        .into_iter()
        .filter(|h| h.score >= floor)
        .take(max_notes)
        .map(|h| UnifiedHit {
            kind: HitKind::Source,
            hit: h,
        })
        .collect();
    Ok(out)
}
