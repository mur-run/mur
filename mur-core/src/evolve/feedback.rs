//! Bayesian importance adjustment based on pattern evidence.

use mur_common::pattern::Pattern;

/// Feedback signal from a session
#[derive(Debug, Clone)]
#[allow(dead_code)] // Success/Override used by hook integration
pub enum FeedbackSignal {
    /// Pattern was injected and the session succeeded
    Success,
    /// Pattern was injected but user overrode/ignored it
    Override,
    /// Manual: user said this pattern is helpful
    Helpful,
    /// Manual: user said this pattern is unhelpful
    Unhelpful,
}

/// Apply a feedback signal to a pattern, updating evidence and importance.
pub fn apply_feedback(pattern: &mut Pattern, signal: FeedbackSignal) {
    let evidence = &mut pattern.evidence;

    match signal {
        FeedbackSignal::Success => {
            evidence.injection_count += 1;
            evidence.success_signals += 1;
        }
        FeedbackSignal::Override => {
            evidence.injection_count += 1;
            evidence.override_signals += 1;
        }
        FeedbackSignal::Helpful => {
            // Manual boost: counts as 2 success signals
            evidence.success_signals += 2;
        }
        FeedbackSignal::Unhelpful => {
            // Manual penalty: counts as 2 override signals
            evidence.override_signals += 2;
        }
    }

    evidence.last_validated = Some(chrono::Utc::now());

    // Bayesian importance update
    pattern.importance = bayesian_update(pattern.importance, &pattern.evidence);
}

/// Bayesian update of importance based on evidence.
///
/// prior = current importance
/// likelihood = effectiveness from recent evidence
/// posterior = weighted blend of prior and likelihood
#[must_use]
fn bayesian_update(prior: f64, evidence: &mur_common::pattern::Evidence) -> f64 {
    let effectiveness = evidence.effectiveness();
    let total = evidence.success_signals + evidence.override_signals;

    // Weight of evidence increases with sample size (max out at ~20 observations)
    let evidence_weight = 1.0 - (-0.1 * total as f64).exp();

    // Blend prior and likelihood based on evidence weight
    let posterior = prior * (1.0 - evidence_weight) + effectiveness * evidence_weight;

    // Clamp to valid range
    posterior.clamp(0.1, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::pattern::*;

    fn make_pattern() -> Pattern {
        Pattern {
            schema: 2,
            name: "test-pattern".into(),
            description: "test".into(),
            content: Content::Plain("test content".into()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.5,
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_success_increases_importance() {
        let mut p = make_pattern();
        let old = p.importance;
        apply_feedback(&mut p, FeedbackSignal::Success);
        assert!(p.importance >= old, "importance should not decrease on success");
        assert_eq!(p.evidence.injection_count, 1);
        assert_eq!(p.evidence.success_signals, 1);
    }

    #[test]
    fn test_override_decreases_importance() {
        let mut p = make_pattern();
        // Give strong initial success history so override pulls it down
        p.evidence.success_signals = 10;
        p.evidence.override_signals = 0;
        p.evidence.injection_count = 10;
        p.importance = 0.9;
        // Apply many overrides to see decrease
        for _ in 0..5 {
            apply_feedback(&mut p, FeedbackSignal::Override);
        }
        assert!(p.importance < 0.9, "importance should decrease after overrides");
    }

    #[test]
    fn test_importance_clamped() {
        let mut p = make_pattern();
        p.importance = 0.05; // below min
        // All overrides
        for _ in 0..20 {
            apply_feedback(&mut p, FeedbackSignal::Override);
        }
        assert!(p.importance >= 0.1, "importance should not go below 0.1");
    }

    #[test]
    fn test_helpful_manual_feedback() {
        let mut p = make_pattern();
        apply_feedback(&mut p, FeedbackSignal::Helpful);
        assert_eq!(p.evidence.success_signals, 2);
        assert_eq!(p.evidence.injection_count, 0); // manual doesn't count as injection
    }

    #[test]
    fn test_bayesian_neutral_prior() {
        let evidence = Evidence {
            success_signals: 5,
            override_signals: 5,
            ..Evidence::default()
        };
        let result = bayesian_update(0.5, &evidence);
        // With 50% effectiveness and 50% prior, should stay around 0.5
        assert!((result - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_bayesian_strong_evidence() {
        let evidence = Evidence {
            success_signals: 20,
            override_signals: 0,
            ..Evidence::default()
        };
        let result = bayesian_update(0.5, &evidence);
        // Strong positive evidence should push importance high
        assert!(result > 0.8);
    }
}
