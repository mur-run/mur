//! Multi-signal scoring pipeline for pattern retrieval.

use std::borrow::Cow;

use chrono::Utc;
use mur_common::config::RetrievalConfig;
use mur_common::pattern::{Origin, Pattern, PatternKind, Tier};

/// Caller-supplied scope context used to make preference/procedure boosts
/// accurate rather than using heuristics like `origin.is_some()`.
#[derive(Debug, Clone, Default)]
pub struct ScoringHints {
    /// The active user identifier (matches `Origin::user`).
    pub user: Option<String>,
    /// The active platform/tool (matches `Origin::platform`).
    pub platform: Option<String>,
    /// The task description — used to detect task-type queries for Procedure boost.
    pub task: Option<String>,
}

/// A retrievable knowledge item. The hybrid scorer is generic over this trait so
/// `Pattern`, `Skill`, and (later) `Note` share one scoring pipeline.
///
/// Default `adjust_score` is the identity; `Pattern` overrides it to apply
/// scope/language/kind boosts so existing Pattern behavior is preserved exactly.
pub trait Retrievable {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn text(&self) -> Cow<'_, str>;
    fn tag_terms(&self) -> Vec<&str>;
    fn importance(&self) -> f64;
    fn effectiveness(&self) -> f64;
    fn tier(&self) -> Tier;
    fn created_at(&self) -> chrono::DateTime<chrono::Utc>;
    fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>>;
    fn decay_half_life_days(&self) -> f64;
    /// Filter predicate: items where this returns false are dropped before scoring.
    fn is_active(&self) -> bool;
    /// Whether this item is a note (`Category::Note`). Drives the reserved
    /// note slots in the budget stage; non-note implementors keep the default.
    fn is_note(&self) -> bool {
        false
    }

    /// Hook for item-specific score adjustment. Default: identity.
    /// Pattern overrides to apply `scope_mult`, `kind_score_boost`, `lang_mult`.
    fn adjust_score(
        &self,
        weighted_sum: f64,
        _query_words: &[&str],
        _scope: Option<&ScoringHints>,
        _project_language: Option<&str>,
    ) -> f64 {
        weighted_sum
    }
}

impl Retrievable for Pattern {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn text(&self) -> Cow<'_, str> {
        self.content.as_text()
    }
    fn tag_terms(&self) -> Vec<&str> {
        self.tags
            .topics
            .iter()
            .chain(self.tags.languages.iter())
            .map(String::as_str)
            .collect()
    }
    fn importance(&self) -> f64 {
        self.importance
    }
    fn effectiveness(&self) -> f64 {
        self.evidence.effectiveness()
    }
    fn tier(&self) -> Tier {
        self.tier
    }
    fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.created_at
    }
    fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.lifecycle
            .last_injected
            .or(self.evidence.last_validated)
    }
    fn decay_half_life_days(&self) -> f64 {
        self.lifecycle
            .decay_half_life
            .unwrap_or_else(|| self.tier.decay_half_life_days()) as f64
    }
    fn is_active(&self) -> bool {
        !self.lifecycle.muted
            && self.lifecycle.status == mur_common::pattern::LifecycleStatus::Active
    }
    fn adjust_score(
        &self,
        weighted_sum: f64,
        query_words: &[&str],
        scope: Option<&ScoringHints>,
        project_language: Option<&str>,
    ) -> f64 {
        let scope_mult = if self.applies.projects.is_empty()
            && self.applies.languages.is_empty()
            && self.applies.tools.is_empty()
        {
            0.7
        } else {
            1.0
        };
        let lang_mult = if let Some(proj_lang) = project_language {
            if !self.applies.languages.is_empty() {
                let proj_lang_lower = proj_lang.to_lowercase();
                let matches = self
                    .applies
                    .languages
                    .iter()
                    .any(|l| l.to_lowercase() == proj_lang_lower);
                if matches { 1.2 } else { 0.05 }
            } else {
                1.0
            }
        } else {
            1.0
        };
        let kind_boost = kind_score_boost(self, query_words, scope);
        (weighted_sum * scope_mult + kind_boost) * lang_mult
    }
}

#[allow(dead_code)]
pub type ScoredSkill = Scored<crate::retrieve::skill_candidates::LoadedSkill>;

/// A retrieved item with its computed relevance score.
#[derive(Debug, Clone)]
pub struct Scored<T> {
    pub item: T,
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

/// Default score floor — patterns below this are dropped
const SCORE_FLOOR: f64 = 0.42;

/// Default max patterns to return
const MAX_PATTERNS: usize = 5;
/// Injection slots held for notes when mature skills would otherwise fill
/// every seat (federation P1). Config override: `retrieval.reserved_note_slots`.
const RESERVED_NOTE_SLOTS: usize = 1;

/// Default max total tokens (rough: 1 token ≈ 4 chars)
const MAX_TOKENS: usize = 2000;

/// Public generic entry point: score and rank any `Vec<T>` where
/// `T: Retrievable`. Keyword-only relevance, no scope, no project_language,
/// default scoring config. Mirrors `score_and_rank` for the generic case.
///
/// Hybrid / scope-aware generic entries are added in later plans as needed.
pub fn score_and_rank_generic<T: Retrievable>(query: &str, candidates: Vec<T>) -> Vec<Scored<T>> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    score_and_rank_inner(
        &query_words,
        candidates,
        None,
        None,
        None,
        |words, item: &T| keyword_relevance(words, item),
    )
}

/// Generic scoring with config-driven retrieval parameters. Same pipeline as
/// `score_and_rank_generic`, with explicit `RetrievalConfig`.
pub fn score_and_rank_generic_with_config<T: Retrievable>(
    query: &str,
    candidates: Vec<T>,
    config: &RetrievalConfig,
) -> Vec<Scored<T>> {
    let query_lower = query.to_lowercase();
    let query_words: Vec<&str> = query_lower.split_whitespace().collect();
    score_and_rank_inner(
        &query_words,
        candidates,
        None,
        None,
        Some(config),
        |words, item: &T| keyword_relevance(words, item),
    )
}

/// Shared scoring logic: filter, score with a relevance function, sort, and budget-limit.
/// Generic over T: Retrievable so Pattern, Skill, and Note share one pipeline.
fn score_and_rank_inner<T, F>(
    query_words: &[&str],
    candidates: Vec<T>,
    scope: Option<&ScoringHints>,
    project_language: Option<&str>,
    config: Option<&RetrievalConfig>,
    relevance_fn: F,
) -> Vec<Scored<T>>
where
    T: Retrievable,
    F: Fn(&[&str], &T) -> f64,
{
    let score_floor = config.map_or(SCORE_FLOOR, |c| c.min_score);
    let max_patterns = config.map_or(MAX_PATTERNS, |c| c.max_patterns);
    let max_tokens = config.map_or(MAX_TOKENS, |c| c.max_tokens);
    let reserved_note_slots = config.map_or(RESERVED_NOTE_SLOTS, |c| c.reserved_note_slots);

    let mut scored: Vec<Scored<T>> = candidates
        .into_iter()
        .filter(Retrievable::is_active)
        .map(|item| {
            let relevance = relevance_fn(query_words, &item);
            let recency = recency_score_for(&item);
            let effectiveness = item.effectiveness();
            let importance = item.importance();
            let time_decay = time_decay_score_for(&item);
            let content_len = item.text().len();
            let length_norm = length_norm_score_from_len(content_len);

            let weighted_sum = relevance * W_RELEVANCE
                + recency * W_RECENCY
                + effectiveness * W_EFFECTIVENESS
                + importance * W_IMPORTANCE
                + time_decay * W_TIME_DECAY
                + length_norm * W_LENGTH_NORM;

            let score = item.adjust_score(weighted_sum, query_words, scope, project_language);

            Scored {
                item,
                score,
                relevance,
            }
        })
        .filter(|sp| sp.score >= score_floor)
        .collect();

    // Sort by score descending, with tier priority as tiebreaker.
    scored.sort_by(|a, b| {
        let score_diff = (a.score - b.score).abs();
        if score_diff < 0.05 {
            tier_priority(&b.item.tier()).cmp(&tier_priority(&a.item.tier()))
        } else {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        }
    });

    // Budget: max items and max tokens. Keep what didn't fit — the note
    // reservation below may promote from it.
    let mut result = Vec::new();
    let mut leftovers = Vec::new();
    let mut token_count = 0;
    let mut budget_full = false;
    for sp in scored {
        if budget_full {
            leftovers.push(sp);
            continue;
        }
        let est_tokens = sp.item.text().len() / 4;
        if result.len() >= max_patterns
            || (token_count + est_tokens > max_tokens && !result.is_empty())
        {
            budget_full = true;
            leftovers.push(sp);
            continue;
        }
        token_count += est_tokens;
        result.push(sp);
    }

    // Reserved note slots (federation P1): mature skills must not permanently
    // outbid notes, or "a Draft note takes effect immediately" is false in
    // practice. When fewer than the reserved number of notes made the cut and
    // eligible notes are in the leftovers, swap out the lowest-scoring
    // non-notes from the tail. Item-for-item swap;
    // ponytail: token budget treats the swap as neutral — revisit only if
    // real note bodies blow the token cap.
    if budget_full && reserved_note_slots > 0 {
        let mut notes_in = result.iter().filter(|s| s.item.is_note()).count();
        let mut promoted: Vec<Scored<T>> = Vec::new();
        for sp in leftovers {
            if notes_in >= reserved_note_slots {
                break;
            }
            if sp.item.is_note() {
                promoted.push(sp);
                notes_in += 1;
            }
        }
        for note in promoted {
            match result.iter().rposition(|s| !s.item.is_note()) {
                Some(pos) => {
                    result.remove(pos);
                    // Score ≤ every kept score, so appending keeps the
                    // descending order intact.
                    result.push(note);
                }
                None => break,
            }
        }
    }
    result
}

/// Pre-computed lowercased fields for a pattern, avoiding repeated allocations
/// in the scoring hot loop.
struct LowerCache {
    name: String,
    description: String,
    content: String,
    tags_text: String,
}

impl LowerCache {
    fn from_item<T: Retrievable + ?Sized>(item: &T) -> Self {
        let tags_text: String = item
            .tag_terms()
            .iter()
            .map(|t| t.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            name: item.name().to_lowercase(),
            description: item.description().to_lowercase(),
            content: item.text().to_lowercase(),
            tags_text,
        }
    }
}

/// Keyword-based relevance (Phase 1, replaced by vector search in Phase 2).
fn keyword_relevance<T: Retrievable + ?Sized>(query_words: &[&str], item: &T) -> f64 {
    if query_words.is_empty() {
        return 0.0;
    }

    let cache = LowerCache::from_item(item);
    keyword_relevance_cached(query_words, &cache)
}

/// Keyword relevance using pre-computed lowercased fields.
fn keyword_relevance_cached(query_words: &[&str], cache: &LowerCache) -> f64 {
    if query_words.is_empty() {
        return 0.0;
    }

    let mut matches = 0;
    for word in query_words {
        if word.len() < 2 {
            continue;
        }
        if cache.name.contains(word) {
            matches += 3; // name match is strongest
        }
        if cache.tags_text.contains(word) {
            matches += 2; // tag match is strong
        }
        if cache.description.contains(word) {
            matches += 2;
        }
        if cache.content.contains(word) {
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

/// Recency score: exp(-days / 14). Generic over Retrievable.
fn recency_score_for<T: Retrievable + ?Sized>(item: &T) -> f64 {
    let last = item.last_activity().unwrap_or_else(|| item.created_at());
    let days = (Utc::now() - last).num_days().max(0) as f64;
    (-days / 14.0).exp()
}

/// Time decay: 0.5 + 0.5 * exp(-days / half_life). Generic over Retrievable.
fn time_decay_score_for<T: Retrievable + ?Sized>(item: &T) -> f64 {
    let half_life = item.decay_half_life_days();
    let last = item.last_activity().unwrap_or_else(|| item.created_at());
    let days = (Utc::now() - last).num_days().max(0) as f64;
    0.5 + 0.5 * (-days / half_life).exp()
}

/// Length normalization: 1 / (1 + 0.5 * log2(len / 500))
fn length_norm_score_from_len(content_len: usize) -> f64 {
    let len = content_len.max(1) as f64;
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

/// Returns true if `scope` matches the pattern's origin user/platform.
/// A match requires that the scope field is present AND the pattern's origin
/// contains the same value.  Missing scope = no scope match (no boost).
#[allow(deprecated)] // transitional: reads legacy origin.user / origin.platform fields
fn scope_matches_origin(scope: Option<&ScoringHints>, origin: Option<&Origin>) -> bool {
    let Some(sc) = scope else { return false };
    let Some(orig) = origin else { return false };

    let user_match = sc
        .user
        .as_deref()
        .map(|u| orig.user.as_deref() == Some(u))
        .unwrap_or(false);
    let platform_match = sc
        .platform
        .as_deref()
        .map(|p| orig.platform.as_deref() == Some(p))
        .unwrap_or(false);

    user_match || platform_match
}

/// Returns true when the query or the task description looks like a how-to request.
fn is_task_query(query_words: &[&str], scope: Option<&ScoringHints>) -> bool {
    const TASK_INDICATORS: &[&str] = &[
        "how",
        "steps",
        "process",
        "deploy",
        "setup",
        "install",
        "build",
        "run",
        "create",
        "configure",
        "guide",
        "tutorial",
    ];
    if query_words.iter().any(|w| TASK_INDICATORS.contains(w)) {
        return true;
    }
    // Also check the explicit task field in scope
    if let Some(sc) = scope
        && let Some(task) = &sc.task
    {
        let task_lower = task.to_lowercase();
        return TASK_INDICATORS.iter().any(|kw| task_lower.contains(kw));
    }
    false
}

/// Score external-source hits. Simpler formula than patterns (no lifecycle,
/// no usage-based decay) — see design spec §8.2.
///
/// Factors:
/// - base = hit.score (combination of vec + BM25 already merged by caller)
/// - source_weight: from user config (default 1.0)
/// - freshness: exp(-age_days / 365)
/// - length_norm: 1.0 − tanh((text_len / 4000) − 1) clamped to [0.5, 1.0]
pub fn score_sources(
    hits: Vec<crate::store::vector::Hit>,
    weights: &std::collections::HashMap<String, f32>,
) -> Vec<crate::store::vector::Hit> {
    let now = chrono::Utc::now();
    let mut scored: Vec<crate::store::vector::Hit> = hits
        .into_iter()
        .map(|mut h| {
            let w = weights.get(&h.source_id).copied().unwrap_or(1.0);
            let age_days = (now - h.updated_at).num_days().max(0) as f32;
            let freshness = (-age_days / 365.0).exp();
            let len_norm = {
                let n = h.text.chars().count() as f32 / 4000.0;
                (1.0 - (n - 1.0).tanh()).clamp(0.5, 1.0)
            };
            h.score *= w * freshness * len_norm;
            h
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    scored
}

/// Kind-aware scoring boost.
///
/// - **Preference / Behavioral**: +0.1 when scope user/platform matches the
///   pattern's origin — i.e., this preference actually belongs to the caller.
///   No boost (0.0) when the scope doesn't match or isn't provided.
/// - **Procedure**: +0.1 when the query or `scope.task` indicates a how-to
///   request.
/// - **Technical / Fact / None**: 0.0 (unchanged).
fn kind_score_boost(pattern: &Pattern, query_words: &[&str], scope: Option<&ScoringHints>) -> f64 {
    match pattern.effective_kind() {
        PatternKind::Preference | PatternKind::Behavioral => {
            // Only boost when the scope actually matches this pattern's origin.
            if scope_matches_origin(scope, pattern.origin.as_ref()) {
                0.1
            } else {
                0.0
            }
        }
        PatternKind::Procedure => {
            if is_task_query(query_words, scope) {
                0.1
            } else {
                0.0
            }
        }
        PatternKind::Technical | PatternKind::Fact => 0.0,
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
}

#[cfg(test)]
mod note_reservation_tests {
    use super::*;

    struct Item {
        name: String,
        note: bool,
        importance: f64,
    }
    impl Retrievable for Item {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "topic thing"
        }
        fn text(&self) -> Cow<'_, str> {
            Cow::Borrowed("topic body content")
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
            Utc::now()
        }
        fn last_activity(&self) -> Option<chrono::DateTime<chrono::Utc>> {
            Some(Utc::now())
        }
        fn decay_half_life_days(&self) -> f64 {
            90.0
        }
        fn is_active(&self) -> bool {
            true
        }
        fn is_note(&self) -> bool {
            self.note
        }
    }

    /// Six max-importance skills + one zero-importance note: the note always
    /// ranks last, so without the reservation it never survives the 5-item cap.
    fn skills_and_note() -> Vec<Item> {
        let mut v: Vec<Item> = (0..6)
            .map(|i| Item {
                name: format!("skill-{i}"),
                note: false,
                importance: 1.0,
            })
            .collect();
        v.push(Item {
            name: "the-note".into(),
            note: true,
            importance: 0.0,
        });
        v
    }

    fn cfg(reserved: usize) -> RetrievalConfig {
        RetrievalConfig {
            min_score: 0.0,
            reserved_note_slots: reserved,
            ..Default::default()
        }
    }

    #[test]
    fn reserved_slot_swaps_in_the_best_note() {
        let r = score_and_rank_generic_with_config("topic", skills_and_note(), &cfg(1));
        assert_eq!(r.len(), 5, "item cap must be unchanged by the swap");
        assert!(
            r.last().unwrap().item.note,
            "the note takes the reserved tail slot"
        );
        assert_eq!(
            r.iter().filter(|s| s.item.note).count(),
            1,
            "exactly the reserved number of notes"
        );
    }

    #[test]
    fn reservation_disabled_keeps_old_behavior() {
        let r = score_and_rank_generic_with_config("topic", skills_and_note(), &cfg(0));
        assert_eq!(r.len(), 5);
        assert!(
            !r.iter().any(|s| s.item.note),
            "reserved_note_slots=0 must opt out entirely"
        );
    }

    #[test]
    fn no_swap_when_a_note_already_made_the_cut() {
        // The note outranks every skill: it earns its seat, nothing is swapped.
        let mut items = skills_and_note();
        items.last_mut().unwrap().importance = 1.0;
        for s in items.iter_mut().take(3) {
            s.importance = 0.0;
        }
        let r = score_and_rank_generic_with_config("topic", items, &cfg(1));
        assert_eq!(r.len(), 5);
        assert_eq!(r.iter().filter(|s| s.item.note).count(), 1);
        assert!(
            !r.last().unwrap().item.note || r.iter().take(4).all(|s| !s.item.note),
            "an organically-placed note is not duplicated by the reservation"
        );
    }

    #[test]
    fn under_cap_result_is_untouched() {
        // Three items only: nothing overflows, reservation is a no-op.
        let items: Vec<Item> = vec![
            Item {
                name: "a".into(),
                note: false,
                importance: 1.0,
            },
            Item {
                name: "b".into(),
                note: true,
                importance: 0.5,
            },
            Item {
                name: "c".into(),
                note: false,
                importance: 0.9,
            },
        ];
        let r = score_and_rank_generic_with_config("topic", items, &cfg(1));
        assert_eq!(r.len(), 3, "all items fit; no swap path runs");
    }
}
