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
fn similarity_of(h: &SearchHit) -> f64 {
    (1.0 - h.distance as f64).clamp(0.0, 1.0)
}

pub async fn gather_hits(args: RetrieveArgs<'_>) -> Result<Vec<ResolvedHit>> {
    let dims = args.query_embedding.len() as i32;
    let idx = ConversationIndex::open(dims, args.root_override).await?;
    let primary_src = args.filters.source.first().copied();

    // Layer 1 (summaries)
    let l1 = idx
        .search(&args.query_embedding, args.k_summary, primary_src, Some(1))
        .await?;
    let top_score = l1.first().map(similarity_of).unwrap_or(0.0);

    // Escalate?
    let l0 = if !args.no_escalate && (top_score < args.escalation_threshold || l1.is_empty()) {
        idx.search(&args.query_embedding, args.k_raw, primary_src, Some(0))
            .await?
    } else {
        Vec::new()
    };

    // Filter by since/until/min_score
    let filtered_l1: Vec<_> = l1.into_iter().filter(|h| passes(h, args.filters)).collect();
    let filtered_l0: Vec<_> = l0.into_iter().filter(|h| passes(h, args.filters)).collect();

    // Resolve snippets
    let mut resolved = Vec::new();
    for h in filtered_l1 {
        resolved.push(resolve_summary_hit(h, args.root_override)?);
    }
    for h in filtered_l0 {
        resolved.push(resolve_raw_hit(h));
    }

    // MMR dedupe on snippet text (simple word-jaccard; reuses Phase 1 filter threshold config by default)
    let deduped = mmr_dedupe(resolved, args.mmr_threshold);

    // Token-budget cap
    let budget = (args.max_context_tokens * 9 / 10).max(400);
    let capped = cap_by_budget(deduped, budget);
    Ok(capped)
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
    }
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
mod tests {
    use super::*;

    #[test]
    fn word_jaccard_identical_is_one() {
        assert_eq!(word_jaccard("a b c", "a b c"), 1.0);
    }

    #[test]
    fn word_jaccard_disjoint_is_zero() {
        assert_eq!(word_jaccard("a b c", "d e f"), 0.0);
    }

    #[test]
    fn mmr_dedupe_drops_duplicate() {
        let h1 = ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "a".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                score: 0.9,
            },
            snippet: "the quick brown fox jumps".into(),
            line_hint: None,
            span_index_in_summary: None,
        };
        let h2 = ResolvedHit {
            snippet: "the quick brown fox jumps".into(),
            info: HitInfo {
                source: "cc".into(),
                conv_id: "b".into(),
                ..h1.info.clone()
            },
            ..h1.clone()
        };
        let out = mmr_dedupe(vec![h1, h2], 0.85);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn cap_by_budget_keeps_at_least_one() {
        let giant = ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "a".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap(),
                score: 0.9,
            },
            snippet: "x".repeat(40_000),
            line_hint: None,
            span_index_in_summary: None,
        };
        let out = cap_by_budget(vec![giant], 100);
        assert_eq!(out.len(), 1, "must keep at least one hit even over budget");
    }
}
