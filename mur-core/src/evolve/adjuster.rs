//! Bayesian importance adjustment based on evidence.
//!
//! ```text
//! prior = current importance
//! likelihood = success_rate from recent injections
//! posterior = (prior * likelihood) / evidence_weight
//! new_importance = clamp(posterior, 0.1, 1.0)
//! ```

use mur_common::pattern::Pattern;

/// Minimum importance value (never drop below)
const MIN_IMPORTANCE: f64 = 0.1;
/// Maximum importance value
const MAX_IMPORTANCE: f64 = 1.0;

/// Adjust a pattern's importance using Bayesian update.
///
/// Returns the new importance value without modifying the pattern.
pub fn bayesian_adjust(pattern: &Pattern) -> f64 {
    let prior = pattern.importance;
    let effectiveness = pattern.evidence.effectiveness();
    let total_signals = pattern.evidence.success_signals + pattern.evidence.override_signals;

    if total_signals == 0 {
        // No evidence yet — keep prior
        return prior;
    }

    // Evidence weight: more signals = more confidence in the likelihood.
    // Starts low (prior dominates), grows toward 1.0 as signals accumulate.
    // At 10 signals, weight ≈ 0.67; at 20, ≈ 0.80; at 50, ≈ 0.91
    let evidence_weight = total_signals as f64 / (total_signals as f64 + 5.0);

    // Weighted blend of prior and observed effectiveness
    let posterior = prior * (1.0 - evidence_weight) + effectiveness * evidence_weight;

    posterior.clamp(MIN_IMPORTANCE, MAX_IMPORTANCE)
}

/// Apply the Bayesian adjustment to a pattern, returning the old and new importance.
pub fn apply_adjustment(pattern: &mut Pattern) -> (f64, f64) {
    let old = pattern.importance;
    let new = bayesian_adjust(pattern);
    pattern.importance = new;
    pattern.updated_at = chrono::Utc::now();
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;

    fn make_pattern(importance: f64, success: u64, overrides: u64) -> Pattern {
        Pattern {
            schema: 2,
            name: "test".to_string(),
            description: "test".to_string(),
            content: Content::DualLayer {
                technical: "test".to_string(),
                principle: None,
            },
            tier: Tier::Session,
            importance,
            confidence: 0.8,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence {
                source_sessions: vec![],
                first_seen: None,
                last_validated: None,
                injection_count: success + overrides,
                success_signals: success,
                override_signals: overrides,
            },
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_no_evidence_keeps_prior() {
        let p = make_pattern(0.7, 0, 0);
        assert_eq!(bayesian_adjust(&p), 0.7);
    }

    #[test]
    fn test_high_effectiveness_increases_importance() {
        // Prior 0.5, 90% effective with 10 signals
        let p = make_pattern(0.5, 9, 1);
        let new = bayesian_adjust(&p);
        assert!(new > 0.5, "Expected increase, got {}", new);
        assert!(new < 1.0);
    }

    #[test]
    fn test_low_effectiveness_decreases_importance() {
        // Prior 0.8, 20% effective with 10 signals
        let p = make_pattern(0.8, 2, 8);
        let new = bayesian_adjust(&p);
        assert!(new < 0.8, "Expected decrease, got {}", new);
        assert!(new >= MIN_IMPORTANCE);
    }

    #[test]
    fn test_never_below_minimum() {
        // Prior 0.1, 0% effective
        let p = make_pattern(0.1, 0, 20);
        let new = bayesian_adjust(&p);
        assert!(new >= MIN_IMPORTANCE, "Got {}", new);
    }

    #[test]
    fn test_never_above_maximum() {
        let p = make_pattern(1.0, 100, 0);
        let new = bayesian_adjust(&p);
        assert!(new <= MAX_IMPORTANCE, "Got {}", new);
    }

    #[test]
    fn test_more_evidence_shifts_toward_likelihood() {
        // Same effectiveness (80%) but different evidence amounts
        let few = make_pattern(0.5, 4, 1); // 5 signals
        let many = make_pattern(0.5, 40, 10); // 50 signals

        let adj_few = bayesian_adjust(&few);
        let adj_many = bayesian_adjust(&many);

        // Both should increase (effectiveness > prior), but many should increase more
        assert!(adj_few > 0.5);
        assert!(adj_many > 0.5);
        assert!(
            adj_many > adj_few,
            "More evidence should shift more: few={}, many={}",
            adj_few,
            adj_many
        );
    }

    #[test]
    fn test_apply_adjustment_mutates() {
        let mut p = make_pattern(0.5, 9, 1);
        let (old, new) = apply_adjustment(&mut p);
        assert_eq!(old, 0.5);
        assert!(new > 0.5);
        assert_eq!(p.importance, new);
    }
}
