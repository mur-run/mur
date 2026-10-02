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
mod tests;

#[cfg(test)]
mod note_reservation_tests;
