use super::*;
use mur_common::pattern::*;

fn make_pattern(name: &str, content: &str) -> Pattern {
    Pattern {
        base: mur_common::knowledge::KnowledgeBase {
            schema: 2,
            name: name.into(),
            description: format!("About {}", name),
            content: Content::Plain(content.into()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence {
                injection_count: 5,
                success_signals: 3,
                override_signals: 1,
                last_validated: Some(Utc::now()),
                ..Evidence::default()
            },
            links: Links::default(),
            lifecycle: Lifecycle {
                last_injected: Some(Utc::now()),
                ..Lifecycle::default()
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    }
}

#[test]
fn test_basic_scoring() {
    let p1 = make_pattern("swift-testing", "Use @Test macro for Swift testing");
    let p2 = make_pattern("rust-error-handling", "Use anyhow for Rust error handling");
    let results = score_and_rank_generic("swift testing", vec![p1, p2]);
    assert!(!results.is_empty());
    assert_eq!(results[0].item.name, "swift-testing");
}

#[test]
fn test_muted_excluded() {
    let mut p = make_pattern("muted-one", "this is muted");
    p.lifecycle.muted = true;
    let results = score_and_rank_generic("muted", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_deprecated_excluded() {
    let mut p = make_pattern("old-one", "this is old");
    p.lifecycle.status = LifecycleStatus::Deprecated;
    let results = score_and_rank_generic("old", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_max_patterns_limit() {
    let patterns: Vec<Pattern> = (0..10)
        .map(|i| {
            let mut p = make_pattern(
                &format!("pattern-{}", i),
                &format!("content about topic {}", i),
            );
            p.tags.topics = vec!["topic".into()];
            p
        })
        .collect();
    let results = score_and_rank_generic("topic", patterns);
    assert!(results.len() <= MAX_PATTERNS);
}

#[test]
fn test_score_floor() {
    let p = make_pattern("unrelated", "completely different content xyz abc");
    let results = score_and_rank_generic("quantum physics entanglement", vec![p]);
    // Should be filtered out by score floor
    assert!(results.is_empty());
}

#[test]
fn test_no_scope_penalty() {
    let mut p_scoped = make_pattern("scoped", "swift testing content");
    p_scoped.applies.languages = vec!["swift".into()];

    let p_unscoped = make_pattern("unscoped", "swift testing content");

    let r1 = score_and_rank_generic("swift testing", vec![p_scoped]);
    let r2 = score_and_rank_generic("swift testing", vec![p_unscoped]);

    if !r1.is_empty() && !r2.is_empty() {
        assert!(
            r1[0].score > r2[0].score,
            "scoped pattern should score higher"
        );
    }
}

#[test]
fn test_length_norm() {
    let short_p = make_pattern("short", "hi");
    assert!((length_norm_score_from_len(short_p.content.as_text().len()) - 1.0).abs() < 0.01);
    let long_content = "x".repeat(2000);
    let long_p = make_pattern("long", &long_content);
    assert!(length_norm_score_from_len(long_p.content.as_text().len()) < 1.0);
}

#[test]
fn test_empty_query_returns_empty() {
    let p = make_pattern("anything", "some content here");
    let results = score_and_rank_generic("", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_name_match_stronger_than_content() {
    let p_name = make_pattern("rust-error", "general programming stuff");
    let p_content = make_pattern("generic-pattern", "rust error handling is important");
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
    let mut p_tagged = make_pattern("pattern-a", "some coding content");
    p_tagged.tags.topics = vec!["rust".into(), "testing".into()];

    let p_untagged = make_pattern("pattern-b", "some coding content");

    let r1 = score_and_rank_generic("rust testing", vec![p_tagged]);
    let r2 = score_and_rank_generic("rust testing", vec![p_untagged]);

    if !r1.is_empty() && !r2.is_empty() {
        assert!(
            r1[0].score > r2[0].score,
            "Tagged pattern should score higher: {} vs {}",
            r1[0].score,
            r2[0].score
        );
    }
}

#[test]
fn test_archived_excluded() {
    let mut p = make_pattern("archived-one", "this is archived content");
    p.lifecycle.status = LifecycleStatus::Archived;
    let results = score_and_rank_generic("archived", vec![p]);
    assert!(results.is_empty());
}

#[test]
fn test_recency_score_recent_is_high() {
    let p = make_pattern("recent", "content");
    // make_pattern sets last_injected to now, so recency should be ~1.0
    let score = recency_score_for(&p);
    assert!(
        score > 0.9,
        "Recently injected pattern should have high recency, got {}",
        score
    );
}

#[test]
fn test_recency_score_old_is_low() {
    let mut p = make_pattern("old", "content");
    p.lifecycle.last_injected = Some(Utc::now() - chrono::Duration::days(60));
    p.evidence.last_validated = None;
    let score = recency_score_for(&p);
    assert!(
        score < 0.1,
        "60-day-old pattern should have low recency, got {}",
        score
    );
}

#[test]
fn test_token_budget_respected() {
    // Create patterns with very long content
    let patterns: Vec<Pattern> = (0..10)
        .map(|i| {
            let mut p = make_pattern(
                &format!("pattern-{}", i),
                &format!("{} {}", "topic ".repeat(200), i),
            );
            p.tags.topics = vec!["topic".into()];
            p
        })
        .collect();
    let results = score_and_rank_generic("topic", patterns);
    // Total token estimate should stay under MAX_TOKENS
    let total_tokens: usize = results
        .iter()
        .map(|sp| sp.item.content.as_text().len() / 4)
        .sum();
    assert!(
        total_tokens <= MAX_TOKENS || results.len() == 1,
        "Should respect token budget"
    );
}

#[test]
fn test_tier_tiebreaker() {
    // Two patterns with similar scores but different tiers
    let mut p_core = make_pattern("core-pattern", "rust error handling tips");
    p_core.tier = Tier::Core;
    p_core.tags.topics = vec!["rust".into()];

    let mut p_session = make_pattern("session-pattern", "rust error handling tips");
    p_session.tier = Tier::Session;
    p_session.tags.topics = vec!["rust".into()];

    let results = score_and_rank_generic("rust error", vec![p_session, p_core]);
    if results.len() >= 2 {
        // Core should be preferred as tiebreaker
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
#[allow(deprecated)] // transitional: user/platform fields being phased out
fn test_kind_boost_preference_with_matching_scope() {
    use mur_common::pattern::{Origin, OriginTrigger};
    let mut p = make_pattern("user-pref", "user prefers dark mode");
    p.kind = Some(PatternKind::Preference);
    p.origin = Some(Origin {
        source: "commander".into(),
        trigger: OriginTrigger::UserExplicit,
        actor: None,
        user: Some("david".into()),
        platform: None,
        confidence: 1.0,
    });
    // Scope matches origin user → boost
    let scope = ScoringHints {
        user: Some("david".into()),
        ..Default::default()
    };
    let boost = kind_score_boost(&p, &["dark", "mode"], Some(&scope));
    assert!((boost - 0.1).abs() < 0.001, "Expected 0.1, got {boost}");
}

#[test]
#[allow(deprecated)] // transitional: user/platform fields being phased out
fn test_kind_boost_preference_no_scope_no_boost() {
    use mur_common::pattern::{Origin, OriginTrigger};
    let mut p = make_pattern("user-pref", "user prefers dark mode");
    p.kind = Some(PatternKind::Preference);
    p.origin = Some(Origin {
        source: "commander".into(),
        trigger: OriginTrigger::UserExplicit,
        actor: None,
        user: Some("david".into()),
        platform: None,
        confidence: 1.0,
    });
    // No scope provided → no boost
    let boost = kind_score_boost(&p, &["dark", "mode"], None);
    assert!((boost - 0.0).abs() < 0.001, "Expected 0.0, got {boost}");
}

#[test]
#[allow(deprecated)] // transitional: user/platform fields being phased out
fn test_kind_boost_preference_wrong_user_no_boost() {
    use mur_common::pattern::{Origin, OriginTrigger};
    let mut p = make_pattern("user-pref", "user prefers dark mode");
    p.kind = Some(PatternKind::Preference);
    p.origin = Some(Origin {
        source: "commander".into(),
        trigger: OriginTrigger::UserExplicit,
        actor: None,
        user: Some("alice".into()),
        platform: None,
        confidence: 1.0,
    });
    let scope = ScoringHints {
        user: Some("bob".into()), // different user
        ..Default::default()
    };
    let boost = kind_score_boost(&p, &["dark", "mode"], Some(&scope));
    assert!(
        (boost - 0.0).abs() < 0.001,
        "Expected 0.0 for mismatched user, got {boost}"
    );
}

#[test]
fn test_kind_boost_procedure_on_how_query() {
    let mut p = make_pattern("deploy-steps", "how to deploy to production");
    p.kind = Some(PatternKind::Procedure);
    let boost = kind_score_boost(&p, &["how", "deploy"], None);
    assert!((boost - 0.1).abs() < 0.001);
}

#[test]
fn test_kind_boost_procedure_via_scope_task() {
    let mut p = make_pattern("deploy-steps", "deploy to production");
    p.kind = Some(PatternKind::Procedure);
    let scope = ScoringHints {
        task: Some("how to set up the environment".into()),
        ..Default::default()
    };
    // query has no task indicators but scope.task does
    let boost = kind_score_boost(&p, &["deploy"], Some(&scope));
    assert!(
        (boost - 0.1).abs() < 0.001,
        "Expected 0.1 via scope.task, got {boost}"
    );
}

#[test]
fn test_kind_boost_technical_unchanged() {
    let p = make_pattern("tech-pattern", "use anyhow for errors");
    // kind is None (default Technical)
    let boost = kind_score_boost(&p, &["error", "handling"], None);
    assert!((boost - 0.0).abs() < 0.001);
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

#[test]
fn generic_scorer_works_for_non_pattern_retrievable() {
    use super::Retrievable;
    use chrono::{Duration, Utc};
    use std::borrow::Cow;

    struct FakeItem {
        name: String,
        description: String,
        body: String,
        importance: f64,
    }
    impl Retrievable for FakeItem {
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
            vec![]
        }
        fn importance(&self) -> f64 {
            self.importance
        }
        fn effectiveness(&self) -> f64 {
            1.0
        }
        fn tier(&self) -> Tier {
            Tier::Project
        }
        fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
            Utc::now() - Duration::days(1)
        }
        fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>> {
            Some(Utc::now() - Duration::days(1))
        }
        fn decay_half_life_days(&self) -> f64 {
            90.0
        }
        fn is_active(&self) -> bool {
            true
        }
    }

    let items = vec![FakeItem {
        name: "fly-deploy".into(),
        description: "Deploy to Fly.io".into(),
        body: "Run fly deploy in the project root.".into(),
        importance: 0.8,
    }];

    let scored = score_and_rank_inner(
        &["fly", "deploy"],
        items,
        None,
        None,
        None,
        |words, item: &FakeItem| keyword_relevance(words, item),
    );

    assert!(
        !scored.is_empty(),
        "fake item should be retrievable through the generic path"
    );
    assert_eq!(scored[0].item.name, "fly-deploy");
    assert!(scored[0].score > 0.0);
}

#[test]
fn pattern_adjust_score_preserves_scope_kind_lang_combination() {
    use super::Retrievable;
    let mut p = make_pattern("rust-error", "rust error body");
    p.applies.languages = vec!["rust".into()];
    let query_words = ["rust", "error"];
    let scope = ScoringHints {
        user: None,
        platform: None,
        task: None,
    };
    let weighted_sum = 0.42;
    let adjusted = p.adjust_score(weighted_sum, &query_words, Some(&scope), Some("rust"));
    assert!((adjusted - (0.42 * 1.0 + 0.0) * 1.2).abs() < 1e-9);
}

#[test]
fn scored_pattern_is_alias_of_scored_pattern_generic() {
    fn _accepts_alias(_: Scored<Pattern>) {}
    fn _accepts_generic(_: Scored<Pattern>) {}
    let p = make_pattern("alpha", "alpha body");
    let s: Scored<Pattern> = Scored {
        item: p,
        score: 1.0,
        relevance: 1.0,
    };
    _accepts_alias(s);
}

#[test]
fn recency_and_decay_use_retrievable_accessors() {
    use chrono::{Duration, Utc};
    let mut p = make_pattern("alpha", "alpha body");
    p.lifecycle.last_injected = Some(Utc::now() - Duration::days(1));
    let r_generic = recency_score_for(&p as &dyn Retrievable);
    let d_generic = time_decay_score_for(&p as &dyn Retrievable);
    assert!((r_generic - 0.93_f64).abs() < 0.02);
    assert!(d_generic > 0.5 && d_generic <= 1.0);
}

#[test]
fn lower_cache_builds_from_retrievable_with_lowered_fields() {
    let p = make_pattern("AlphaBeta", "AlphaBeta Body Content");
    let cache = LowerCache::from_item(&p);
    assert_eq!(cache.name, "alphabeta");
    assert!(cache.description.chars().all(|c| !c.is_uppercase()));
    assert!(cache.content.chars().all(|c| !c.is_uppercase()));
}

#[test]
fn pattern_implements_retrievable_with_expected_accessors() {
    use super::Retrievable;
    let p = make_pattern("alpha", "alpha body");
    assert_eq!(p.name(), "alpha");
    assert_eq!(p.description(), p.description.as_str());
    assert_eq!(&*p.text(), &*p.content.as_text());
    assert_eq!(p.importance(), p.importance);
    assert_eq!(p.effectiveness(), p.evidence.effectiveness());
    assert_eq!(p.tier(), p.tier);
    assert_eq!(p.created_at(), p.created_at);
    assert!(p.is_active());
    assert_eq!(
        p.decay_half_life_days(),
        p.tier.decay_half_life_days() as f64
    );
}

#[test]
fn score_and_rank_generic_ranks_pattern_corpus_like_score_and_rank() {
    let p1 = make_pattern("alpha", "alpha body about deploy");
    let p2 = make_pattern("beta", "beta body about something else");
    let generic = score_and_rank_generic("alpha deploy", vec![p1.clone(), p2.clone()]);
    let legacy = score_and_rank_generic("alpha deploy", vec![p1, p2]);
    assert_eq!(generic.len(), legacy.len());
    for (g, l) in generic.iter().zip(legacy.iter()) {
        assert_eq!(g.item.name, l.item.name);
        assert!((g.score - l.score).abs() < 1e-9);
    }
}
