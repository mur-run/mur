//! Difficulty heuristic for the cost-router.
//!
//! Scores sub-tasks 0.0–1.0 based on task type, context size, and
//! keyword signals. The router uses this score to decide local vs
//! frontier routing.

use mur_common::route::TaskType;

/// Scores a sub-task's difficulty 0.0 (trivial) to 1.0 (needs frontier).
pub trait DifficultyHeuristic {
    /// Compute a difficulty score.
    ///
    /// * `task_summary` — human-readable description of the work.
    /// * `task_type` — classified task category.
    /// * `estimated_tokens` — rough context-window size estimate.
    fn score(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
    ) -> f64;
}

/// Default difficulty heuristic.
///
/// Combines:
/// 1. Base score from `TaskType::base_difficulty()` (`WEIGHT_BASE`)
/// 2. Context-size factor (`WEIGHT_CONTEXT`)
/// 3. Keyword boost (`WEIGHT_KEYWORD`)
///
/// The three weights sum to 1.0, so each factor in [0,1] yields a score in
/// [0.0, 1.0] (clamped). Realistic max ≈ 0.85 (highest `base_difficulty` is
/// `Refactor` at 0.70), which keeps `PREFER_LOCAL_THRESHOLD` (0.75) reachable.
#[derive(Debug, Clone)]
pub struct DefaultHeuristic {
    /// Tokens at which the context factor hits 1.0.
    pub max_context_tokens: u64,
    /// Tokens below which context contribution is ~0.
    pub min_context_tokens: u64,
}

impl Default for DefaultHeuristic {
    fn default() -> Self {
        Self {
            max_context_tokens: 100_000,
            min_context_tokens: 100,
        }
    }
}

/// Weight of the task-type base difficulty in the final score.
const WEIGHT_BASE: f64 = 0.50;
/// Weight of the log-scale context-size factor.
const WEIGHT_CONTEXT: f64 = 0.35;
/// Weight of the keyword-complexity boost.
const WEIGHT_KEYWORD: f64 = 0.15;
/// Number of high-complexity keyword matches that saturate the boost at 1.0.
/// (Invariant: `WEIGHT_BASE + WEIGHT_CONTEXT + WEIGHT_KEYWORD == 1.0`.)
const KEYWORD_SATURATION: usize = 3;

impl DefaultHeuristic {
    /// Keywords that signal high complexity and boost the score.
    const HIGH_COMPLEXITY_KEYWORDS: &'static [&'static str] = &[
        "refactor",
        "rewrite",
        "redesign",
        "architect",
        "migrate",
        "multi-file",
        "cross-cutting",
        "breaking change",
        "backward compat",
        "concurrency",
        "race condition",
        "deadlock",
        "security audit",
        "vulnerability",
        "optimize",
        "performance critical",
    ];

    /// Normalized [0.0, 1.0] keyword signal: rises linearly with the number of
    /// high-complexity keywords present, saturating at `KEYWORD_SATURATION`.
    /// `score` multiplies this by `WEIGHT_KEYWORD`.
    fn keyword_boost(&self, summary: &str) -> f64 {
        let lower = summary.to_lowercase();
        let matches = Self::HIGH_COMPLEXITY_KEYWORDS
            .iter()
            .filter(|kw| lower.contains(&kw.to_lowercase()))
            .count();
        (matches as f64 / KEYWORD_SATURATION as f64).min(1.0)
    }

    fn context_factor(&self, estimated_tokens: u64) -> f64 {
        if estimated_tokens <= self.min_context_tokens {
            return 0.0;
        }
        if estimated_tokens >= self.max_context_tokens {
            return 1.0;
        }
        // Log-scale interpolation so the curve rises quickly at first then
        // flattens.  Small tasks stay cheap; large tasks ramp toward 1.0.
        let log_min = (self.min_context_tokens as f64).ln();
        let log_max = (self.max_context_tokens as f64).ln();
        let log_val = (estimated_tokens as f64).ln();
        let raw = (log_val - log_min) / (log_max - log_min);
        raw.clamp(0.0, 1.0)
    }
}

impl DifficultyHeuristic for DefaultHeuristic {
    fn score(
        &self,
        task_summary: &str,
        task_type: TaskType,
        estimated_tokens: u64,
    ) -> f64 {
        let base = task_type.base_difficulty();
        let context = self.context_factor(estimated_tokens);
        let keywords = self.keyword_boost(task_summary);

        let raw = WEIGHT_BASE * base + WEIGHT_CONTEXT * context + WEIGHT_KEYWORD * keywords;
        raw.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_scores_low() {
        let h = DefaultHeuristic::default();
        let score = h.score("run cargo test", TaskType::Execution, 200);
        assert!(score < 0.5, "execution should score low, got {score}");
    }

    #[test]
    fn refactor_scores_high() {
        let h = DefaultHeuristic::default();
        let score = h.score(
            "refactor the auth module to use the new token format",
            TaskType::Refactor,
            5000,
        );
        assert!(score > 0.5, "refactor should score high, got {score}");
    }

    #[test]
    fn more_tokens_increases_score() {
        let h = DefaultHeuristic::default();
        let small = h.score("fix typo in README", TaskType::Documentation, 100);
        let large = h.score("fix typo in README", TaskType::Documentation, 10000);
        assert!(large > small, "more tokens should increase score: {small} vs {large}");
    }

    #[test]
    fn keyword_boost_works() {
        let h = DefaultHeuristic::default();
        let without = h.score("change the color", TaskType::CodeGen, 500);
        let with = h.score("refactor and rewrite the auth module", TaskType::CodeGen, 500);
        assert!(with > without, "keywords should boost: {without} vs {with}");
    }

    #[test]
    fn score_clamped_to_one() {
        let h = DefaultHeuristic::default();
        let score = h.score(
            "redesign architecture migrate security audit",
            TaskType::Refactor,
            200_000,
        );
        assert!((0.0..=1.0).contains(&score), "score {score} out of range");
    }

    #[test]
    fn context_factor_log_scale() {
        let h = DefaultHeuristic::default();
        // 100 tokens → ~0, 100k tokens → ~1.0
        let low = h.context_factor(100);
        let mid = h.context_factor(10_000);
        let high = h.context_factor(100_000);
        assert!(low < 0.1, "low={low}");
        assert!(mid > 0.4, "mid={mid}");
        assert!(high > 0.95, "high={high}");
        assert!(mid - low > 0.2, "log scale should give separation");
    }
}
