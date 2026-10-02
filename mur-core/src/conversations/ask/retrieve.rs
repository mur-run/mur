//! Tiered retrieval (spec §5.2).
//! Stages: embed → layer=1 search → escalate to layer=0 if top score low
//! → MMR dedupe → per-hit snippet resolution → token-budget cap.

use anyhow::Result;

use super::super::index::{ConversationIndex, SearchHit};
use super::super::summarize;
use super::{Filters, HitInfo};

#[derive(Debug, Clone)]
pub struct ResolvedHit {
    pub layer: i8,
    pub info: HitInfo,
    pub snippet: String,
    pub line_hint: Option<u32>,
    pub span_index_in_summary: Option<u32>,
    pub vector: Option<Vec<f32>>,
    pub compressed: Option<super::Compression>,
}

pub struct RetrieveArgs<'a> {
    pub query_embedding: Vec<f32>,
    pub filters: &'a Filters,
    pub k_summary: usize,
    pub k_raw: usize,
    pub escalation_threshold: f64,
    pub mmr_threshold: f64,
    pub no_escalate: bool,
    pub max_context_tokens: usize,
    pub root_override: Option<&'a str>,
}

/// LanceDB cosine distance = 1 - cosine_similarity.
/// Converts to similarity (higher = better, range 0..1).
pub(crate) fn similarity_of(h: &SearchHit) -> f64 {
    (1.0 - h.distance as f64).clamp(0.0, 1.0)
}

pub async fn gather_hits(args: RetrieveArgs<'_>) -> Result<Vec<ResolvedHit>> {
    let dims = args.query_embedding.len() as i32;
    let idx = ConversationIndex::open(dims, args.root_override).await?;
    // Note: --src filtering via `primary_src` applies only to day-level
    // content (layers 0/1/2). Layer=3/4 rollup rows are multi-source
    // aggregates and always surface based on relevance — see the l3/l4
    // search calls below which pass None instead of `primary_src`.
    // (Phase 3.2.1 fix — prior Phase 3.2 behavior silently dropped all
    // rollup hits under --src; see docs/superpowers/specs/2026-04-22-mur-conversations-phase-3-2-1-design.md §5.)
    let primary_src = args.filters.source.first().copied();

    // Phase 3.2: collapsed tree — one k-NN per layer {2,1,3,4}, merged.
    let k_each = (args.k_summary as u32).div_ceil(4).max(1) as usize;
    let l2 = idx
        .search(&args.query_embedding, k_each, primary_src, Some(2))
        .await?;
    let l1 = idx
        .search(&args.query_embedding, k_each, primary_src, Some(1))
        .await?;
    // Phase 3.2.1: rollup rows are multi-source aggregates by construction
    // (built from day summaries across all enabled sources). The --src filter
    // applies only to day-level content (layers 0/1/2); rollups surface based
    // purely on embedding relevance. Pass None so the LanceDB predicate
    // doesn't exclude them via source-column mismatch.
    let l3 = idx
        .search(&args.query_embedding, k_each, None, Some(3))
        .await?;
    let l4 = idx
        .search(&args.query_embedding, k_each, None, Some(4))
        .await?;

    let upper_empty = l2.is_empty() && l1.is_empty() && l3.is_empty() && l4.is_empty();
    let effective_top = [&l2, &l1, &l3, &l4]
        .iter()
        .filter_map(|v| v.first())
        .map(similarity_of)
        .fold(0.0_f64, f64::max);
    let l0 = if !args.no_escalate && (upper_empty || effective_top < args.escalation_threshold) {
        idx.search(&args.query_embedding, args.k_raw, primary_src, Some(0))
            .await?
    } else {
        Vec::new()
    };

    let mut resolved: Vec<ResolvedHit> = Vec::new();
    for h in l2.into_iter().filter(|h| passes(h, args.filters)) {
        resolved.push(resolve_span_hit(h)?);
    }
    for h in l1.into_iter().filter(|h| passes(h, args.filters)) {
        resolved.push(resolve_summary_hit(h, args.root_override)?);
    }
    for h in l3.into_iter().filter(|h| passes(h, args.filters)) {
        resolved.push(resolve_week_hit(h, args.root_override)?);
    }
    for h in l4.into_iter().filter(|h| passes(h, args.filters)) {
        resolved.push(resolve_month_hit(h, args.root_override)?);
    }
    for h in l0.into_iter().filter(|h| passes(h, args.filters)) {
        resolved.push(resolve_raw_hit(h));
    }

    // Global score sort so mixed-layer MMR picks the highest-scoring hit first.
    resolved.sort_by(|a, b| {
        b.info
            .score
            .partial_cmp(&a.info.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let deduped = mmr_dedupe_cosine(resolved, args.mmr_threshold);
    let budget = (args.max_context_tokens * 9 / 10).max(400);
    Ok(cap_by_budget(deduped, budget))
}

fn passes(h: &SearchHit, f: &Filters) -> bool {
    if similarity_of(h) < f.min_score {
        return false;
    }
    if let Some(s) = f.since
        && chrono::DateTime::from_timestamp(h.ts, 0)
            .map(|dt| dt.date_naive() < s)
            .unwrap_or(false)
    {
        return false;
    }
    if let Some(u) = f.until
        && chrono::DateTime::from_timestamp(h.ts, 0)
            .map(|dt| dt.date_naive() > u)
            .unwrap_or(false)
    {
        return false;
    }
    true
}

fn resolve_summary_hit(h: SearchHit, root_override: Option<&str>) -> Result<ResolvedHit> {
    // Read summary file for h.date, pick the first extractive span's text.
    // (Phase 2B simplification; Phase 3 RAPTOR improves this.)
    let date = chrono::DateTime::from_timestamp(h.ts, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    let (md_path, _) = super::super::paths::summary_paths_for(date, root_override);
    let (snippet, line_hint, span_idx) = if md_path.exists() {
        let body = std::fs::read_to_string(&md_path).unwrap_or_default();
        if let Ok(parsed) = summarize::parse_summary(&body) {
            parsed.extractive.first().map_or_else(
                || (String::new(), None, None),
                |s| (s.text.clone(), Some(s.line_hint), Some(s.span_index)),
            )
        } else {
            (String::new(), None, None)
        }
    } else {
        (String::new(), None, None)
    };
    Ok(ResolvedHit {
        layer: 1,
        info: HitInfo {
            layer: 1,
            source: h.source.file_prefix().to_string(),
            conv_id: h.conv_id.clone(),
            date,
            score: similarity_of(&h),
        },
        snippet,
        line_hint,
        span_index_in_summary: span_idx,
        vector: h.vector,
        compressed: None,
    })
}

fn resolve_raw_hit(h: SearchHit) -> ResolvedHit {
    let date = chrono::DateTime::from_timestamp(h.ts, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    ResolvedHit {
        layer: 0,
        info: HitInfo {
            layer: 0,
            source: h.source.file_prefix().to_string(),
            conv_id: h.conv_id.clone(),
            date,
            score: similarity_of(&h),
        },
        snippet: h.content.clone(),
        line_hint: None, // raw hits don't carry line hints; extensible in Phase 3
        span_index_in_summary: None,
        vector: h.vector,
        compressed: None,
    }
}

fn resolve_span_hit(h: SearchHit) -> Result<ResolvedHit> {
    let line_hint =
        h.id.rsplit_once("_L2_")
            .and_then(|(_, suffix)| suffix.parse::<u32>().ok());
    let date = chrono::DateTime::from_timestamp(h.ts, 0)
        .map(|d| d.date_naive())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    Ok(ResolvedHit {
        layer: 2,
        info: HitInfo {
            layer: 2,
            source: h.source.file_prefix().to_string(),
            conv_id: h.conv_id.clone(),
            date,
            score: similarity_of(&h),
        },
        snippet: h.content.clone(),
        line_hint,
        span_index_in_summary: line_hint,
        vector: h.vector,
        compressed: None,
    })
}

fn resolve_week_hit(h: SearchHit, _root_override: Option<&str>) -> Result<ResolvedHit> {
    let window_label = h
        .conv_id
        .strip_prefix("week:")
        .unwrap_or(&h.conv_id)
        .to_string();
    let monday = crate::conversations::summarize::windows::iso_week_monday(&window_label)
        .ok()
        .or_else(|| chrono::DateTime::from_timestamp(h.ts, 0).map(|d| d.date_naive()))
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    let score = similarity_of(&h);
    Ok(ResolvedHit {
        layer: 3,
        info: HitInfo {
            layer: 3,
            source: "week".to_string(),
            conv_id: window_label,
            date: monday,
            score,
        },
        snippet: h.content.clone(),
        line_hint: None,
        span_index_in_summary: None,
        vector: h.vector,
        compressed: None,
    })
}

fn resolve_month_hit(h: SearchHit, _root_override: Option<&str>) -> Result<ResolvedHit> {
    let window_label = h
        .conv_id
        .strip_prefix("month:")
        .unwrap_or(&h.conv_id)
        .to_string();
    let first = crate::conversations::summarize::windows::month_first_day(&window_label)
        .ok()
        .or_else(|| chrono::DateTime::from_timestamp(h.ts, 0).map(|d| d.date_naive()))
        .unwrap_or_else(|| chrono::Utc::now().date_naive());
    let score = similarity_of(&h);
    Ok(ResolvedHit {
        layer: 4,
        info: HitInfo {
            layer: 4,
            source: "month".to_string(),
            conv_id: window_label,
            date: first,
            score,
        },
        snippet: h.content.clone(),
        line_hint: None,
        span_index_in_summary: None,
        vector: h.vector,
        compressed: None,
    })
}

fn mmr_dedupe(hits: Vec<ResolvedHit>, threshold: f64) -> Vec<ResolvedHit> {
    let mut kept: Vec<ResolvedHit> = Vec::new();
    for h in hits {
        let dup = kept
            .iter()
            .any(|k| word_jaccard(&k.snippet, &h.snippet) > threshold);
        if !dup {
            kept.push(h);
        }
    }
    kept
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x * *y) as f64;
        na += (*x * *x) as f64;
        nb += (*y * *y) as f64;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn similar(a: &ResolvedHit, b: &ResolvedHit, threshold: f64) -> bool {
    match (&a.vector, &b.vector) {
        (Some(av), Some(bv)) => cosine_sim(av, bv) > threshold,
        _ => word_jaccard(&a.snippet, &b.snippet) > threshold,
    }
}

pub(crate) fn mmr_dedupe_cosine(hits: Vec<ResolvedHit>, threshold: f64) -> Vec<ResolvedHit> {
    let mut kept: Vec<ResolvedHit> = Vec::new();
    for h in hits {
        let dup = kept.iter().any(|k| similar(&h, k, threshold));
        if !dup {
            kept.push(h);
        }
    }
    kept
}

fn word_jaccard(a: &str, b: &str) -> f64 {
    use std::collections::HashSet;
    let sa: HashSet<&str> = a.split_whitespace().collect();
    let sb: HashSet<&str> = b.split_whitespace().collect();
    if sa.is_empty() && sb.is_empty() {
        return 1.0;
    }
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn cap_by_budget(hits: Vec<ResolvedHit>, budget_tokens: usize) -> Vec<ResolvedHit> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for h in hits {
        let est = (h.snippet.len() + 80) / 4 + 1;
        if used + est > budget_tokens && !out.is_empty() {
            break;
        }
        used += est;
        out.push(h);
    }
    out
}

#[cfg(test)]
mod tests;
