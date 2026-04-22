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
mod tests {
    use super::super::super::index::SearchHit;
    use super::*;
    use mur_common::Source;

    #[test]
    fn cosine_sim_identical_is_one() {
        let v = vec![0.1, 0.2, 0.3, 0.4];
        assert!((cosine_sim(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_orthogonal_is_zero() {
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 0.0];
        assert!(cosine_sim(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_sim_zero_length_does_not_panic() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_sim(&a, &b), 0.0);
    }

    #[test]
    fn cosine_sim_mismatched_length_returns_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_sim(&a, &b), 0.0);
    }

    #[test]
    fn resolve_span_hit_parses_line_hint_from_id() {
        let h = SearchHit {
            id: "cc_abc_L2_17".into(),
            ts: 0,
            source: Source::ClaudeCode,
            conv_id: "abc".into(),
            content: "hello".into(),
            distance: 0.1,
            layer: 2,
            vector: Some(vec![0.1; 16]),
        };
        let r = resolve_span_hit(h).unwrap();
        assert_eq!(r.line_hint, Some(17));
        assert_eq!(r.span_index_in_summary, Some(17));
        assert_eq!(r.layer, 2);
        assert_eq!(r.snippet, "hello");
    }

    #[test]
    fn resolve_span_hit_without_l2_suffix_has_no_line_hint() {
        let h = SearchHit {
            id: "cc_abc_7".into(),
            ts: 0,
            source: Source::ClaudeCode,
            conv_id: "abc".into(),
            content: "x".into(),
            distance: 0.5,
            layer: 2,
            vector: None,
        };
        let r = resolve_span_hit(h).unwrap();
        assert_eq!(r.line_hint, None);
    }

    #[test]
    fn mmr_dedupe_cosine_drops_near_duplicate() {
        let a_vec = vec![1.0, 0.0, 0.0, 0.0];
        let b_vec = vec![0.99, 0.01, 0.0, 0.0];
        let mk = |v: Vec<f32>, conv: &str| ResolvedHit {
            layer: 2,
            info: HitInfo {
                layer: 2,
                source: "cc".into(),
                conv_id: conv.into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                score: 0.9,
            },
            snippet: format!("text-{conv}"),
            line_hint: Some(1),
            span_index_in_summary: Some(1),
            vector: Some(v),
        };
        let out = mmr_dedupe_cosine(vec![mk(a_vec, "a"), mk(b_vec, "b")], 0.88);
        assert_eq!(out.len(), 1, "near-duplicate should drop to 1");
    }

    #[test]
    fn mmr_dedupe_cosine_keeps_diverse_hits() {
        let a_vec = vec![1.0, 0.0, 0.0, 0.0];
        let b_vec = vec![0.0, 1.0, 0.0, 0.0];
        let mk = |v: Vec<f32>, conv: &str| ResolvedHit {
            layer: 2,
            info: HitInfo {
                layer: 2,
                source: "cc".into(),
                conv_id: conv.into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                score: 0.9,
            },
            snippet: format!("text-{conv}"),
            line_hint: Some(1),
            span_index_in_summary: Some(1),
            vector: Some(v),
        };
        let out = mmr_dedupe_cosine(vec![mk(a_vec, "a"), mk(b_vec, "b")], 0.88);
        assert_eq!(out.len(), 2, "orthogonal vectors should both survive");
    }

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
            vector: None,
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
            vector: None,
        };
        let out = cap_by_budget(vec![giant], 100);
        assert_eq!(out.len(), 1, "must keep at least one hit even over budget");
    }

    fn make_msg(conv: &str, text: &str) -> mur_common::Message {
        mur_common::Message {
            v: 1,
            ts: chrono::Utc::now(),
            src: mur_common::Source::ClaudeCode,
            conv: conv.into(),
            role: mur_common::Role::User,
            content: mur_common::Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn resolve_week_hit_strips_conv_prefix_and_derives_monday() {
        use chrono::TimeZone;
        let h = SearchHit {
            id: "wk_2026-W16_L3_0".into(),
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 13, 0, 0, 0)
                .unwrap()
                .timestamp(),
            source: Source::ClaudeCode,
            conv_id: "week:2026-W16".into(),
            content: "this week...".into(),
            distance: 0.1,
            layer: 3,
            vector: Some(vec![0.1; 16]),
        };
        let r = resolve_week_hit(h, None).unwrap();
        assert_eq!(r.layer, 3);
        assert_eq!(r.info.conv_id, "2026-W16");
        assert_eq!(r.info.source, "week");
        assert_eq!(
            r.info.date,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 13).unwrap()
        );
        assert_eq!(r.snippet, "this week...");
    }

    #[test]
    fn resolve_month_hit_strips_conv_prefix_and_derives_1st() {
        use chrono::TimeZone;
        let h = SearchHit {
            id: "mo_2026-04_L4_0".into(),
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 1, 0, 0, 0)
                .unwrap()
                .timestamp(),
            source: Source::ClaudeCode,
            conv_id: "month:2026-04".into(),
            content: "this month...".into(),
            distance: 0.1,
            layer: 4,
            vector: Some(vec![0.1; 16]),
        };
        let r = resolve_month_hit(h, None).unwrap();
        assert_eq!(r.layer, 4);
        assert_eq!(r.info.conv_id, "2026-04");
        assert_eq!(r.info.source, "month");
        assert_eq!(
            r.info.date,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()
        );
    }

    #[tokio::test]
    async fn gather_hits_prefers_layer_2() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = super::super::super::index::ConversationIndex::open(16, Some(root))
            .await
            .unwrap();
        let s = make_msg("c_span", "span text");
        idx.upsert_with_layer(&[(s, vec![0.7; 16], 2)])
            .await
            .unwrap();
        let args = RetrieveArgs {
            query_embedding: vec![0.7; 16],
            filters: &Filters {
                source: vec![],
                since: None,
                until: None,
                min_score: 0.0,
            },
            k_summary: 4,
            k_raw: 4,
            escalation_threshold: 0.3,
            mmr_threshold: 0.95,
            no_escalate: false,
            max_context_tokens: 6000,
            root_override: Some(root),
        };
        let hits = gather_hits(args).await.unwrap();
        // Phase 3.2: collapsed tree surfaces hits from all populated layers.
        // Layer=2 is no longer "preferred" — it's one of the four parallel
        // searches. Assert layer=2 is AMONG the returned layers.
        assert!(
            hits.iter().any(|h| h.layer == 2),
            "layer=2 should appear in results"
        );
    }

    #[tokio::test]
    async fn gather_hits_falls_back_to_layer_1_when_no_spans() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = super::super::super::index::ConversationIndex::open(16, Some(root))
            .await
            .unwrap();
        let s = make_msg("c_summary", "narrative text");
        idx.upsert_with_layer(&[(s, vec![0.7; 16], 1)])
            .await
            .unwrap();
        let args = RetrieveArgs {
            query_embedding: vec![0.7; 16],
            filters: &Filters {
                source: vec![],
                since: None,
                until: None,
                min_score: 0.0,
            },
            k_summary: 4,
            k_raw: 4,
            escalation_threshold: 0.3,
            mmr_threshold: 0.95,
            no_escalate: false,
            max_context_tokens: 6000,
            root_override: Some(root),
        };
        let hits = gather_hits(args).await.unwrap();
        assert!(
            hits.iter().any(|h| h.layer == 1),
            "layer=1 should appear in results"
        );
    }

    #[tokio::test]
    async fn gather_hits_collapsed_tree_returns_hits_from_multiple_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = super::super::super::index::ConversationIndex::open(16, Some(root))
            .await
            .unwrap();
        // Use distinct vectors per layer so MMR (cosine) doesn't dedupe them.
        // vec_l2: mostly dim-0, vec_l3: mostly dim-1, vec_l4: mostly dim-2
        let mut vec_l2 = vec![0.0f32; 16];
        vec_l2[0] = 1.0;
        let mut vec_l3 = vec![0.0f32; 16];
        vec_l3[1] = 1.0;
        let mut vec_l4 = vec![0.0f32; 16];
        vec_l4[2] = 1.0;
        // Query vector is mix of all three so all are found via k-NN
        let mut query = vec![0.0f32; 16];
        query[0] = 1.0;
        query[1] = 1.0;
        query[2] = 1.0;

        // Seed layer=2 span
        let s = make_msg("c_span", "span text");
        idx.upsert_with_layer(&[(s, vec_l2.clone(), 2)])
            .await
            .unwrap();
        // Seed layer=3 week
        idx.upsert_rollup_row(super::super::super::index::RollupRow {
            id: "wk_2026-W16_L3_0",
            ts: 0,
            source: "week",
            conv_id: "week:2026-W16",
            layer: 3,
            content: "week narrative",
            vector: &vec_l3,
        })
        .await
        .unwrap();
        // Seed layer=4 month
        idx.upsert_rollup_row(super::super::super::index::RollupRow {
            id: "mo_2026-04_L4_0",
            ts: 0,
            source: "month",
            conv_id: "month:2026-04",
            layer: 4,
            content: "month narrative",
            vector: &vec_l4,
        })
        .await
        .unwrap();

        let args = RetrieveArgs {
            query_embedding: query,
            filters: &Filters {
                source: vec![],
                since: None,
                until: None,
                min_score: 0.0,
            },
            k_summary: 8,
            k_raw: 4,
            escalation_threshold: 0.3,
            // threshold=0.95: orthogonal vectors have cosine=0.0 < 0.95 so all 3 survive MMR
            mmr_threshold: 0.95,
            no_escalate: false,
            max_context_tokens: 6000,
            root_override: Some(root),
        };
        let hits = gather_hits(args).await.unwrap();
        let layers: Vec<i8> = hits.iter().map(|h| h.layer).collect();
        assert!(layers.contains(&2), "layers: {layers:?}");
        assert!(layers.contains(&3), "layers: {layers:?}");
        assert!(layers.contains(&4), "layers: {layers:?}");
    }

    #[tokio::test]
    async fn gather_hits_escalates_to_layer_0_when_all_upper_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = super::super::super::index::ConversationIndex::open(16, Some(root))
            .await
            .unwrap();
        let m = make_msg("raw", "raw message");
        idx.upsert_with_layer(&[(m, vec![0.5; 16], 0)])
            .await
            .unwrap();
        let args = RetrieveArgs {
            query_embedding: vec![0.5; 16],
            filters: &Filters {
                source: vec![],
                since: None,
                until: None,
                min_score: 0.0,
            },
            k_summary: 4,
            k_raw: 4,
            escalation_threshold: 0.5,
            mmr_threshold: 0.95,
            no_escalate: false,
            max_context_tokens: 6000,
            root_override: Some(root),
        };
        let hits = gather_hits(args).await.unwrap();
        assert!(
            hits.iter().any(|h| h.layer == 0),
            "expected layer=0 via escalation; got: {:?}",
            hits.iter().map(|h| h.layer).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn gather_hits_rollup_surfaces_despite_src_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut idx = super::super::super::index::ConversationIndex::open(16, Some(root))
            .await
            .unwrap();

        // Seed one layer=2 cc span + one layer=3 rollup ("week" synthetic source).
        // Use different vectors so MMR doesn't dedupe them based on cosine similarity.
        let mut vec_l2 = vec![0.0f32; 16];
        vec_l2[0] = 1.0; // mostly dim-0
        let mut vec_l3 = vec![0.0f32; 16];
        vec_l3[1] = 1.0; // mostly dim-1
        let s = make_msg("c_span", "span text");
        idx.upsert_with_layer(&[(s, vec_l2.clone(), 2)])
            .await
            .unwrap();
        idx.upsert_rollup_row(super::super::super::index::RollupRow {
            id: "wk_2026-W16_L3_0",
            ts: 0,
            source: "week",
            conv_id: "week:2026-W16",
            layer: 3,
            content: "week narrative",
            vector: &vec_l3,
        })
        .await
        .unwrap();

        // Query with --src cc filter active (would exclude layer=3 rows pre-3.2.1).
        // Use a query vector that finds both layer=2 and layer=3
        let mut query = vec![0.0f32; 16];
        query[0] = 1.0; // finds layer=2
        query[1] = 1.0; // finds layer=3
        let args = RetrieveArgs {
            query_embedding: query,
            filters: &Filters {
                source: vec![Source::ClaudeCode],
                since: None,
                until: None,
                min_score: 0.0,
            },
            k_summary: 8,
            k_raw: 4,
            escalation_threshold: 0.3,
            mmr_threshold: 0.95,
            no_escalate: false,
            max_context_tokens: 6000,
            root_override: Some(root),
        };
        let hits = gather_hits(args).await.unwrap();
        let layers: Vec<i8> = hits.iter().map(|h| h.layer).collect();
        assert!(
            layers.contains(&2),
            "cc layer=2 span must survive source filter; layers: {layers:?}"
        );
        assert!(
            layers.contains(&3),
            "layer=3 rollup must surface despite --src filter (Phase 3.2.1); layers: {layers:?}"
        );
    }
}
