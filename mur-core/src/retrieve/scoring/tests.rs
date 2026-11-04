use super::*;
use chrono::{DateTime, Duration, Utc};
use std::borrow::Cow;

/// Minimal `Retrievable` fixture so the scorer is tested through the trait it
/// is generic over, not through any one knowledge type.
#[derive(Clone)]
struct Item {
    name: String,
    description: String,
    body: String,
    tags: Vec<String>,
    tier: Tier,
    importance: f64,
    active: bool,
    created_at: DateTime<Utc>,
    last_activity: Option<DateTime<Utc>>,
    half_life_days: f64,
}

fn make_item(name: &str, body: &str) -> Item {
    Item {
        name: name.into(),
        description: format!("About {name}"),
        body: body.into(),
        tags: vec![],
        tier: Tier::Session,
        importance: 0.5,
        active: true,
        created_at: Utc::now(),
        last_activity: Some(Utc::now()),
        half_life_days: Tier::Session.decay_half_life_days() as f64,
    }
}

impl Retrievable for Item {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn text(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.body)
    }
    fn tag_terms(&self) -> Vec<&str> {
        self.tags.iter().map(String::as_str).collect()
    }
    fn importance(&self) -> f64 {
        self.importance
    }
    fn effectiveness(&self) -> f64 {
        // Low enough that a zero-relevance item sits under `SCORE_FLOOR`, so
        // the floor and empty-query tests exercise the filter.
        0.3
    }
    fn tier(&self) -> Tier {
        self.tier
    }
    fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    fn last_activity(&self) -> Option<DateTime<Utc>> {
        self.last_activity
    }
    fn decay_half_life_days(&self) -> f64 {
        self.half_life_days
    }
    fn is_active(&self) -> bool {
        self.active
    }
}

#[test]
fn test_basic_scoring() {
    let p1 = make_item("swift-testing", "Use @Test macro for Swift testing");
    let p2 = make_item("rust-error-handling", "Use anyhow for Rust error handling");
    let results = score_and_rank_generic("swift testing", vec![p1, p2]);
    assert!(!results.is_empty());
    assert_eq!(results[0].item.name, "swift-testing");
}

#[test]
fn test_inactive_excluded() {
    let mut p = make_item("old-one", "this is old");
    p.active = false;
    let results = score_and_rank_generic("old", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_max_items_limit() {
    let items: Vec<Item> = (0..10)
        .map(|i| {
            let mut p = make_item(&format!("item-{i}"), &format!("content about topic {i}"));
            p.tags = vec!["topic".into()];
            p
        })
        .collect();
    let results = score_and_rank_generic("topic", items);
    assert!(results.len() <= MAX_PATTERNS);
}

#[test]
fn test_score_floor() {
    let p = make_item("unrelated", "completely different content xyz abc");
    let results = score_and_rank_generic("quantum physics entanglement", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_length_norm() {
    let short_p = make_item("short", "hi");
    assert!((length_norm_score_from_len(short_p.body.len()) - 1.0).abs() < 0.01);
    let long_p = make_item("long", &"x".repeat(2000));
    assert!(length_norm_score_from_len(long_p.body.len()) < 1.0);
}

#[test]
fn test_empty_query_returns_empty() {
    let p = make_item("anything", "some content here");
    let results = score_and_rank_generic("", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_name_match_stronger_than_content() {
    let p_name = make_item("rust-error", "general programming stuff");
    let p_content = make_item("generic-item", "rust error handling is important");
    let results = score_and_rank_generic("rust error", vec![p_name, p_content]);
    if results.len() >= 2 {
        assert_eq!(
            results[0].item.name, "rust-error",
            "Name match should rank higher"
        );
    }
}

#[test]
fn test_tag_match_boosts_score() {
    let mut p_tagged = make_item("item-a", "some coding content");
    p_tagged.tags = vec!["rust".into(), "testing".into()];
    let p_untagged = make_item("item-b", "some coding content");

    let r1 = score_and_rank_generic("rust testing", vec![p_tagged]);
    let r2 = score_and_rank_generic("rust testing", vec![p_untagged]);

    if !r1.is_empty() && !r2.is_empty() {
        assert!(
            r1[0].score > r2[0].score,
            "Tagged item should score higher: {} vs {}",
            r1[0].score,
            r2[0].score
        );
    }
}

#[test]
fn test_recency_score_recent_is_high() {
    let p = make_item("recent", "content");
    let score = recency_score_for(&p);
    assert!(
        score > 0.9,
        "Recently used item should have high recency, got {score}"
    );
}

#[test]
fn test_recency_score_old_is_low() {
    let mut p = make_item("old", "content");
    p.last_activity = Some(Utc::now() - Duration::days(60));
    let score = recency_score_for(&p);
    assert!(
        score < 0.1,
        "60-day-old item should have low recency, got {score}"
    );
}

#[test]
fn test_token_budget_respected() {
    let items: Vec<Item> = (0..10)
        .map(|i| {
            let mut p = make_item(
                &format!("item-{i}"),
                &format!("{} {i}", "topic ".repeat(200)),
            );
            p.tags = vec!["topic".into()];
            p
        })
        .collect();
    let results = score_and_rank_generic("topic", items);
    let total_tokens: usize = results.iter().map(|sp| sp.item.body.len() / 4).sum();
    assert!(
        total_tokens <= MAX_TOKENS || results.len() == 1,
        "Should respect token budget"
    );
}

#[test]
fn test_tier_tiebreaker() {
    let mut p_core = make_item("core-item", "rust error handling tips");
    p_core.tier = Tier::Core;
    p_core.tags = vec!["rust".into()];

    let mut p_session = make_item("session-item", "rust error handling tips");
    p_session.tier = Tier::Session;
    p_session.tags = vec!["rust".into()];

    let results = score_and_rank_generic("rust error", vec![p_session, p_core]);
    if results.len() >= 2 {
        let first_tier = &results[0].item.tier;
        let second_tier = &results[1].item.tier;
        let score_diff = (results[0].score - results[1].score).abs();
        if score_diff < 0.05 {
            assert_eq!(
                tier_priority(first_tier),
                3,
                "Core tier should win tiebreak"
            );
            assert!(tier_priority(first_tier) >= tier_priority(second_tier));
        }
    }
}

#[test]
fn recency_and_decay_use_retrievable_accessors() {
    let mut p = make_item("alpha", "alpha body");
    p.last_activity = Some(Utc::now() - Duration::days(1));
    let r_generic = recency_score_for(&p as &dyn Retrievable);
    let d_generic = time_decay_score_for(&p as &dyn Retrievable);
    assert!((r_generic - 0.93_f64).abs() < 0.02);
    assert!(d_generic > 0.5 && d_generic <= 1.0);
}

#[test]
fn lower_cache_builds_from_retrievable_with_lowered_fields() {
    let p = make_item("AlphaBeta", "AlphaBeta Body Content");
    let cache = LowerCache::from_item(&p);
    assert_eq!(cache.name, "alphabeta");
    assert!(cache.description.chars().all(|c| !c.is_uppercase()));
    assert!(cache.content.chars().all(|c| !c.is_uppercase()));
}

#[test]
fn generic_scorer_ranks_through_score_and_rank_inner() {
    let items = vec![Item {
        importance: 0.8,
        ..make_item("fly-deploy", "Run fly deploy in the project root.")
    }];
    let scored = score_and_rank_inner(&["fly", "deploy"], items, None, |words, item: &Item| {
        keyword_relevance(words, item)
    });
    assert!(
        !scored.is_empty(),
        "item should be retrievable through the generic path"
    );
    assert_eq!(scored[0].item.name, "fly-deploy");
    assert!(scored[0].score > 0.0);
}

#[test]
fn score_sources_applies_source_weight() {
    use super::score_sources;
    use crate::store::vector::Hit;
    let now = chrono::Utc::now();
    let hits = vec![
        Hit {
            chunk_id: "a".into(),
            source_id: "s:low".into(),
            external_id: "d1".into(),
            score: 0.8,
            text: "some text".into(),
            heading_path: vec![],
            updated_at: now,
        },
        Hit {
            chunk_id: "b".into(),
            source_id: "s:high".into(),
            external_id: "d2".into(),
            score: 0.8,
            text: "some text".into(),
            heading_path: vec![],
            updated_at: now,
        },
    ];
    let weights = [
        ("s:low".to_string(), 0.3_f32),
        ("s:high".to_string(), 2.0_f32),
    ]
    .into_iter()
    .collect();
    let out = score_sources(hits, &weights);
    assert_eq!(out[0].source_id, "s:high");
    assert_eq!(out[1].source_id, "s:low");
    assert!(out[0].score > out[1].score);
}

#[test]
fn score_sources_freshness_penalises_old() {
    use super::score_sources;
    use crate::store::vector::Hit;
    let recent = chrono::Utc::now();
    let old = recent - chrono::Duration::days(365 * 3);
    let hits = vec![
        Hit {
            chunk_id: "a".into(),
            source_id: "s".into(),
            external_id: "new".into(),
            score: 0.5,
            text: "x".into(),
            heading_path: vec![],
            updated_at: recent,
        },
        Hit {
            chunk_id: "b".into(),
            source_id: "s".into(),
            external_id: "old".into(),
            score: 0.5,
            text: "x".into(),
            heading_path: vec![],
            updated_at: old,
        },
    ];
    let weights = std::collections::HashMap::new();
    let out = score_sources(hits, &weights);
    assert_eq!(out[0].external_id, "new");
}
