//! Bayesian importance adjustment based on pattern evidence.

use mur_common::pattern::Pattern;

/// Feedback signal from a session
#[derive(Debug, Clone)]
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

/// Apply a feedback signal to a pattern, updating evidence, importance, and confidence.
pub fn apply_feedback(pattern: &mut Pattern, signal: FeedbackSignal) {
    let now = chrono::Utc::now();

    match signal {
        FeedbackSignal::Success => {
            pattern.evidence.injection_count += 1;
            pattern.evidence.success_signals += 1;
        }
        FeedbackSignal::Override => {
            pattern.evidence.injection_count += 1;
            pattern.evidence.override_signals += 1;
        }
        FeedbackSignal::Helpful => {
            // Manual boost: counts as 2 success signals
            pattern.evidence.success_signals += 2;
            // Confidence boost: +0.05, capped at 1.0
            pattern.confidence = (pattern.confidence + 0.05).min(1.0);
        }
        FeedbackSignal::Unhelpful => {
            // Manual penalty: counts as 2 override signals
            pattern.evidence.override_signals += 2;
            // Confidence penalty: -0.10, floored at 0.0
            pattern.confidence = (pattern.confidence - 0.10).max(0.0);
        }
    }

    pattern.evidence.last_validated = Some(now);
    // Touch decay.last_active on any feedback
    pattern.decay.last_active = Some(now);

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
            base: mur_common::knowledge::KnowledgeBase {
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
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    #[test]
    fn test_success_increases_importance() {
        let mut p = make_pattern();
        let old = p.importance;
        apply_feedback(&mut p, FeedbackSignal::Success);
        assert!(
            p.importance >= old,
            "importance should not decrease on success"
        );
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
        assert!(
            p.importance < 0.9,
            "importance should decrease after overrides"
        );
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

    #[test]
    fn test_helpful_boosts_confidence() {
        let mut p = make_pattern();
        p.confidence = 0.5;
        apply_feedback(&mut p, FeedbackSignal::Helpful);
        assert!(
            (p.confidence - 0.55).abs() < 0.001,
            "Helpful should boost confidence by 0.05, got {}",
            p.confidence
        );
    }

    #[test]
    fn test_unhelpful_penalizes_confidence() {
        let mut p = make_pattern();
        p.confidence = 0.5;
        apply_feedback(&mut p, FeedbackSignal::Unhelpful);
        assert!(
            (p.confidence - 0.40).abs() < 0.001,
            "Unhelpful should decrease confidence by 0.10, got {}",
            p.confidence
        );
    }

    #[test]
    fn test_confidence_capped_at_one() {
        let mut p = make_pattern();
        p.confidence = 0.98;
        apply_feedback(&mut p, FeedbackSignal::Helpful);
        assert!(
            (p.confidence - 1.0).abs() < 0.001,
            "Confidence should cap at 1.0, got {}",
            p.confidence
        );
    }

    #[test]
    fn test_confidence_floored_at_zero() {
        let mut p = make_pattern();
        p.confidence = 0.05;
        apply_feedback(&mut p, FeedbackSignal::Unhelpful);
        assert!(
            p.confidence.abs() < 0.001,
            "Confidence should floor at 0.0, got {}",
            p.confidence
        );
    }

    #[test]
    fn test_feedback_touches_last_active() {
        let mut p = make_pattern();
        assert!(p.decay.last_active.is_none());
        apply_feedback(&mut p, FeedbackSignal::Success);
        assert!(
            p.decay.last_active.is_some(),
            "Feedback should set decay.last_active"
        );
    }

    #[test]
    fn test_success_no_confidence_change() {
        let mut p = make_pattern();
        p.confidence = 0.5;
        apply_feedback(&mut p, FeedbackSignal::Success);
        assert!(
            (p.confidence - 0.5).abs() < 0.001,
            "Success should not change confidence directly, got {}",
            p.confidence
        );
    }
}
