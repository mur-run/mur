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
        compressed: None,
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
        compressed: None,
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
        compressed: None,
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
        compressed: None,
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
