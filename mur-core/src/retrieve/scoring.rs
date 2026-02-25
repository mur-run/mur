//! Multi-signal scoring pipeline for pattern retrieval.

use chrono::Utc;
use mur_common::pattern::{Pattern, Tier};

/// A pattern with its computed relevance score.
#[derive(Debug, Clone)]
pub struct ScoredPattern {
    pub pattern: Pattern,
    pub score: f64,
    pub relevance: f64,
}

/// Scoring weights (from PLAN.md)
const W_RELEVANCE: f64 = 0.45;
const W_RECENCY: f64 = 0.10;
const W_EFFECTIVENESS: f64 = 0.15;
const W_IMPORTANCE: f64 = 0.15;
const W_TIME_DECAY: f64 = 0.10;
const W_LENGTH_NORM: f64 = 0.05;

/// Score floor — patterns below this are dropped
const SCORE_FLOOR: f64 = 0.35;

/// Max patterns to return
const MAX_PATTERNS: usize = 5;

/// Max total tokens (rough: 1 token ≈ 4 chars)
const MAX_TOKENS: usize = 2000;

/// Score patterns with hybrid search (vector + keyword).
/// `vector_scores` maps pattern name → vector similarity (0-1).
pub fn score_and_rank_hybrid(
    query: &str,
    candidates: Vec<Pattern>,
    vector_scores: &std::collections::HashMap<String, f64>,
) -> Vec<ScoredPattern> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();

    score_and_rank_inner(&query_words, candidates, |words, p| {
        let kw_relevance = keyword_relevance(words, p);
        let vec_relevance = vector_scores.get(&p.name).copied().unwrap_or(0.0);
        vec_relevance * 0.7 + kw_relevance * 0.3
    })
}

/// Score a set of candidate patterns against a query (keyword-only fallback).
/// Returns scored patterns sorted by score, filtered and budget-limited.
pub fn score_and_rank(query: &str, candidates: Vec<Pattern>) -> Vec<ScoredPattern> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();

    score_and_rank_inner(&query_words, candidates, |words, p| {
        keyword_relevance(words, p)
    })
}

/// Shared scoring logic: filter, score with a relevance function, sort, and budget-limit.
fn score_and_rank_inner<F>(
    query_words: &[&str],
    candidates: Vec<Pattern>,
    relevance_fn: F,
) -> Vec<ScoredPattern>
where
    F: Fn(&[&str], &Pattern) -> f64,
{
    let mut scored: Vec<ScoredPattern> = candidates
        .into_iter()
        .filter(|p| !p.lifecycle.muted)
        .filter(|p| p.lifecycle.status == mur_common::pattern::LifecycleStatus::Active)
        .map(|p| {
            let relevance = relevance_fn(query_words, &p);
            let recency = recency_score(&p);
            let effectiveness = p.evidence.effectiveness();
            let importance = p.importance;
            let time_decay = time_decay_score(&p);
            let length_norm = length_norm_score(&p);

            let scope_mult = if p.applies.projects.is_empty()
                && p.applies.languages.is_empty()
                && p.applies.tools.is_empty()
            {
                0.7
            } else {
                1.0
            };

            let score = (relevance * W_RELEVANCE
                + recency * W_RECENCY
                + effectiveness * W_EFFECTIVENESS
                + importance * W_IMPORTANCE
                + time_decay * W_TIME_DECAY
                + length_norm * W_LENGTH_NORM)
                * scope_mult;

            ScoredPattern {
                pattern: p,
                score,
                relevance,
            }
        })
        .filter(|sp| sp.score >= SCORE_FLOOR)
        .collect();

    // Sort by score descending, with tier priority as tiebreaker
    scored.sort_by(|a, b| {
        let score_diff = (a.score - b.score).abs();
        if score_diff < 0.05 {
            tier_priority(&b.pattern.tier).cmp(&tier_priority(&a.pattern.tier))
        } else {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    // Budget: max patterns and max tokens
    let mut result = Vec::new();
    let mut token_count = 0;
    for sp in scored {
        if result.len() >= MAX_PATTERNS {
            break;
        }
        let content_len = sp.pattern.content.as_text().len();
        let est_tokens = content_len / 4;
        if token_count + est_tokens > MAX_TOKENS && !result.is_empty() {
            break;
        }
        token_count += est_tokens;
        result.push(sp);
    }

    result
}

/// Keyword-based relevance (Phase 1, replaced by vector search in Phase 2).
fn keyword_relevance(query_words: &[&str], pattern: &Pattern) -> f64 {
    if query_words.is_empty() {
        return 0.0;
    }

    let content = pattern.content.as_text().to_lowercase();
    let name = pattern.name.to_lowercase();
    let desc = pattern.description.to_lowercase();
    let tags_text: String = pattern
        .tags
        .topics
        .iter()
        .chain(pattern.tags.languages.iter())
        .map(|t| t.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    let mut matches = 0;
    for word in query_words {
        if word.len() < 2 {
            continue;
        }
        if name.contains(word) {
            matches += 3; // name match is strongest
        }
        if tags_text.contains(word) {
            matches += 2; // tag match is strong
        }
        if desc.contains(word) {
            matches += 2;
        }
        if content.contains(word) {
            matches += 1;
        }
    }

    let max_possible = query_words.len() * 8; // 3+2+2+1 per word
    if max_possible == 0 {
        0.0
    } else {
        (matches as f64 / max_possible as f64).min(1.0)
    }
}

/// Recency score: exp(-days / 14)
fn recency_score(pattern: &Pattern) -> f64 {
    let last = pattern
        .lifecycle
        .last_injected
        .or(pattern.evidence.last_validated)
        .unwrap_or(pattern.created_at);
    let days = (Utc::now() - last).num_days().max(0) as f64;
    (-days / 14.0).exp()
}

/// Time decay: 0.5 + 0.5 * exp(-days / half_life)
fn time_decay_score(pattern: &Pattern) -> f64 {
    let half_life = pattern
        .lifecycle
        .decay_half_life
        .unwrap_or_else(|| pattern.tier.decay_half_life_days()) as f64;
    let last = pattern
        .lifecycle
        .last_injected
        .or(pattern.evidence.last_validated)
        .unwrap_or(pattern.created_at);
    let days = (Utc::now() - last).num_days().max(0) as f64;
    0.5 + 0.5 * (-days / half_life).exp()
}

/// Length normalization: 1 / (1 + 0.5 * log2(len / 500))
fn length_norm_score(pattern: &Pattern) -> f64 {
    let len = pattern.content.as_text().len().max(1) as f64;
    let ratio = len / 500.0;
    if ratio <= 1.0 {
        1.0
    } else {
        1.0 / (1.0 + 0.5 * ratio.log2())
    }
}

fn tier_priority(tier: &Tier) -> u8 {
    match tier {
        Tier::Core => 3,
        Tier::Project => 2,
        Tier::Session => 1,
    }
}

#[cfg(test)]
mod tests {
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
            attachments: vec![],
        }
    }

    #[test]
    fn test_basic_scoring() {
        let p1 = make_pattern("swift-testing", "Use @Test macro for Swift testing");
        let p2 = make_pattern("rust-error-handling", "Use anyhow for Rust error handling");
        let results = score_and_rank("swift testing", vec![p1, p2]);
        assert!(!results.is_empty());
        assert_eq!(results[0].pattern.name, "swift-testing");
    }

    #[test]
    fn test_muted_excluded() {
        let mut p = make_pattern("muted-one", "this is muted");
        p.lifecycle.muted = true;
        let results = score_and_rank("muted", vec![p]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_deprecated_excluded() {
        let mut p = make_pattern("old-one", "this is old");
        p.lifecycle.status = LifecycleStatus::Deprecated;
        let results = score_and_rank("old", vec![p]);
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
        let results = score_and_rank("topic", patterns);
        assert!(results.len() <= MAX_PATTERNS);
    }

    #[test]
    fn test_score_floor() {
        let p = make_pattern("unrelated", "completely different content xyz abc");
        let results = score_and_rank("quantum physics entanglement", vec![p]);
        // Should be filtered out by score floor
        assert!(results.is_empty());
    }

    #[test]
    fn test_no_scope_penalty() {
        let mut p_scoped = make_pattern("scoped", "swift testing content");
        p_scoped.applies.languages = vec!["swift".into()];

        let p_unscoped = make_pattern("unscoped", "swift testing content");

        let r1 = score_and_rank("swift testing", vec![p_scoped]);
        let r2 = score_and_rank("swift testing", vec![p_unscoped]);

        if !r1.is_empty() && !r2.is_empty() {
            assert!(
                r1[0].score > r2[0].score,
                "scoped pattern should score higher"
            );
        }
    }

    #[test]
    fn test_length_norm() {
        assert!((length_norm_score(&make_pattern("short", "hi")) - 1.0).abs() < 0.01);
        let long_content = "x".repeat(2000);
        let long_p = make_pattern("long", &long_content);
        assert!(length_norm_score(&long_p) < 1.0);
    }

    #[test]
    fn test_empty_query_returns_empty() {
        let p = make_pattern("anything", "some content here");
        let results = score_and_rank("", vec![p]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_name_match_stronger_than_content() {
        let p_name = make_pattern("rust-error", "general programming stuff");
        let p_content = make_pattern("generic-pattern", "rust error handling is important");
        let results = score_and_rank("rust error", vec![p_name, p_content]);
        if results.len() >= 2 {
            assert_eq!(
                results[0].pattern.name, "rust-error",
                "Name match should rank higher"
            );
        }
    }

    #[test]
    fn test_tag_match_boosts_score() {
        let mut p_tagged = make_pattern("pattern-a", "some coding content");
        p_tagged.tags.topics = vec!["rust".into(), "testing".into()];

        let p_untagged = make_pattern("pattern-b", "some coding content");

        let r1 = score_and_rank("rust testing", vec![p_tagged]);
        let r2 = score_and_rank("rust testing", vec![p_untagged]);

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
        let results = score_and_rank("archived", vec![p]);
        assert!(results.is_empty());
    }

    #[test]
    fn test_recency_score_recent_is_high() {
        let p = make_pattern("recent", "content");
        // make_pattern sets last_injected to now, so recency should be ~1.0
        let score = recency_score(&p);
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
        let score = recency_score(&p);
        assert!(
            score < 0.1,
            "60-day-old pattern should have low recency, got {}",
            score
        );
    }

    #[test]
    fn test_hybrid_scoring_with_vector_scores() {
        let p1 = make_pattern("swift-testing", "Use @Test macro for Swift testing");
        let p2 = make_pattern("rust-error-handling", "Use anyhow for Rust error handling");

        let mut vector_scores = std::collections::HashMap::new();
        vector_scores.insert("swift-testing".to_string(), 0.9);
        vector_scores.insert("rust-error-handling".to_string(), 0.1);

        let results = score_and_rank_hybrid("swift testing", vec![p1, p2], &vector_scores);
        assert!(!results.is_empty());
        assert_eq!(results[0].pattern.name, "swift-testing");
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
        let results = score_and_rank("topic", patterns);
        // Total token estimate should stay under MAX_TOKENS
        let total_tokens: usize = results
            .iter()
            .map(|sp| sp.pattern.content.as_text().len() / 4)
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

        let results = score_and_rank("rust error", vec![p_session, p_core]);
        if results.len() >= 2 {
            // Core should be preferred as tiebreaker
            let first_tier = &results[0].pattern.tier;
            let second_tier = &results[1].pattern.tier;
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
}
