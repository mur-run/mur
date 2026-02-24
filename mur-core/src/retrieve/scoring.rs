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

/// Score a set of candidate patterns against a query.
/// Returns scored patterns sorted by score, filtered and budget-limited.
pub fn score_and_rank(query: &str, candidates: Vec<Pattern>) -> Vec<ScoredPattern> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();

    let mut scored: Vec<ScoredPattern> = candidates
        .into_iter()
        .filter(|p| !p.lifecycle.muted)
        .filter(|p| p.lifecycle.status == mur_common::pattern::LifecycleStatus::Active)
        .map(|p| {
            let relevance = keyword_relevance(&query_words, &p);
            let recency = recency_score(&p);
            let effectiveness = p.evidence.effectiveness();
            let importance = p.importance;
            let time_decay = time_decay_score(&p);
            let length_norm = length_norm_score(&p);

            // No-scope penalty
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

    // Sort by score descending
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Tier priority: within similar scores, prefer core > project > session
    scored.sort_by(|a, b| {
        let score_diff = (a.score - b.score).abs();
        if score_diff < 0.05 {
            tier_priority(&b.pattern.tier).cmp(&tier_priority(&a.pattern.tier))
        } else {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
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
                let mut p = make_pattern(&format!("pattern-{}", i), &format!("content about topic {}", i));
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
            assert!(r1[0].score > r2[0].score, "scoped pattern should score higher");
        }
    }

    #[test]
    fn test_length_norm() {
        assert!((length_norm_score(&make_pattern("short", "hi")) - 1.0).abs() < 0.01);
        let long_content = "x".repeat(2000);
        let long_p = make_pattern("long", &long_content);
        assert!(length_norm_score(&long_p) < 1.0);
    }
}
