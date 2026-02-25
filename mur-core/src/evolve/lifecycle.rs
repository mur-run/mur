//! Pattern lifecycle management: tier promotion, deprecation, archival.

use chrono::Utc;
use mur_common::pattern::{LifecycleStatus, Pattern, Tier};

/// Result of evaluating a pattern's lifecycle
#[derive(Debug, PartialEq)]
pub enum LifecycleAction {
    /// No change needed
    None,
    /// Promote to higher tier
    Promote(Tier),
    /// Mark as deprecated
    Deprecate,
    /// Move to archive
    Archive,
}

/// Evaluate whether a pattern should be promoted, deprecated, or archived.
pub fn evaluate_lifecycle(pattern: &Pattern) -> LifecycleAction {
    // Pinned patterns are immune to deprecation/archival
    if pattern.lifecycle.pinned {
        // But they can still be promoted
        return evaluate_promotion(pattern).unwrap_or(LifecycleAction::None);
    }

    // Check archival first (deprecated + 180 days)
    if pattern.lifecycle.status == LifecycleStatus::Deprecated {
        if let Some(last) = pattern.lifecycle.last_injected {
            let days = (Utc::now() - last).num_days();
            if days >= 180 {
                return LifecycleAction::Archive;
            }
        }
        // If deprecated but no last_injected, check created_at
        let days = (Utc::now() - pattern.created_at).num_days();
        if days >= 180 {
            return LifecycleAction::Archive;
        }
        return LifecycleAction::None;
    }

    // Check deprecation: 90 days no injection OR effectiveness < 0.3
    if should_deprecate(pattern) {
        return LifecycleAction::Deprecate;
    }

    // Check promotion
    evaluate_promotion(pattern).unwrap_or(LifecycleAction::None)
}

fn should_deprecate(pattern: &Pattern) -> bool {
    if pattern.lifecycle.status != LifecycleStatus::Active {
        return false;
    }

    let effectiveness = pattern.evidence.effectiveness();
    let total_signals = pattern.evidence.success_signals + pattern.evidence.override_signals;

    // Low effectiveness with enough data
    if total_signals >= 5 && effectiveness < 0.3 {
        return true;
    }

    // 90 days no injection
    let last_used = pattern
        .lifecycle
        .last_injected
        .unwrap_or(pattern.created_at);
    let days_since = (Utc::now() - last_used).num_days();
    days_since >= 90
}

/// Evaluate tier promotion rules:
/// - session → project: injection_count >= 5 AND effectiveness >= 0.7
/// - project → core: applies to >= 3 projects AND effectiveness >= 0.8
fn evaluate_promotion(pattern: &Pattern) -> Option<LifecycleAction> {
    let effectiveness = pattern.evidence.effectiveness();

    match pattern.tier {
        Tier::Session => {
            if pattern.evidence.injection_count >= 5 && effectiveness >= 0.7 {
                Some(LifecycleAction::Promote(Tier::Project))
            } else {
                None
            }
        }
        Tier::Project => {
            if pattern.applies.projects.len() >= 3 && effectiveness >= 0.8 {
                Some(LifecycleAction::Promote(Tier::Core))
            } else {
                None
            }
        }
        Tier::Core => None, // Already at highest tier
    }
}

/// Apply a lifecycle action to a pattern (mutates in place).
pub fn apply_lifecycle_action(pattern: &mut Pattern, action: &LifecycleAction) {
    match action {
        LifecycleAction::None => {}
        LifecycleAction::Promote(tier) => {
            pattern.tier = *tier;
            pattern.updated_at = Utc::now();
        }
        LifecycleAction::Deprecate => {
            pattern.lifecycle.status = LifecycleStatus::Deprecated;
            pattern.updated_at = Utc::now();
        }
        LifecycleAction::Archive => {
            pattern.lifecycle.status = LifecycleStatus::Archived;
            pattern.updated_at = Utc::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mur_common::pattern::*;

    fn make_pattern() -> Pattern {
        Pattern {
            base: mur_common::knowledge::KnowledgeBase {
                schema: 2,
                name: "test".into(),
                description: "test".into(),
                content: Content::Plain("test".into()),
                tier: Tier::Session,
                importance: 0.5,
                confidence: 0.5,
                tags: Tags::default(),
                applies: Applies::default(),
                evidence: Evidence::default(),
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    #[test]
    fn test_session_to_project_promotion() {
        let mut p = make_pattern();
        p.evidence.injection_count = 5;
        p.evidence.success_signals = 4;
        p.evidence.override_signals = 1; // 80% effectiveness
        assert_eq!(
            evaluate_lifecycle(&p),
            LifecycleAction::Promote(Tier::Project)
        );
    }

    #[test]
    fn test_session_not_promoted_low_count() {
        let mut p = make_pattern();
        p.evidence.injection_count = 3; // too few
        p.evidence.success_signals = 3;
        // But created recently so no deprecation either
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::None);
    }

    #[test]
    fn test_session_not_promoted_low_effectiveness() {
        let mut p = make_pattern();
        p.evidence.injection_count = 10;
        p.evidence.success_signals = 5;
        p.evidence.override_signals = 5; // 50% < 70%
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::None);
    }

    #[test]
    fn test_project_to_core_promotion() {
        let mut p = make_pattern();
        p.tier = Tier::Project;
        p.applies.projects = vec!["a".into(), "b".into(), "c".into()];
        p.evidence.success_signals = 10;
        p.evidence.override_signals = 1; // ~91%
        p.evidence.injection_count = 11;
        p.lifecycle.last_injected = Some(Utc::now());
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::Promote(Tier::Core));
    }

    #[test]
    fn test_deprecation_low_effectiveness() {
        let mut p = make_pattern();
        p.evidence.success_signals = 1;
        p.evidence.override_signals = 5; // 16.7% < 30%
        p.lifecycle.last_injected = Some(Utc::now()); // recent, so only effectiveness triggers
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::Deprecate);
    }

    #[test]
    fn test_deprecation_no_injection_90_days() {
        let mut p = make_pattern();
        p.created_at = Utc::now() - Duration::days(100);
        // No last_injected, created 100 days ago
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::Deprecate);
    }

    #[test]
    fn test_pinned_immune_to_deprecation() {
        let mut p = make_pattern();
        p.lifecycle.pinned = true;
        p.created_at = Utc::now() - Duration::days(200);
        // Would be deprecated without pin
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::None);
    }

    #[test]
    fn test_archive_after_deprecated_180_days() {
        let mut p = make_pattern();
        p.lifecycle.status = LifecycleStatus::Deprecated;
        p.created_at = Utc::now() - Duration::days(200);
        assert_eq!(evaluate_lifecycle(&p), LifecycleAction::Archive);
    }

    #[test]
    fn test_apply_promote() {
        let mut p = make_pattern();
        apply_lifecycle_action(&mut p, &LifecycleAction::Promote(Tier::Project));
        assert_eq!(p.tier, Tier::Project);
    }

    #[test]
    fn test_apply_deprecate() {
        let mut p = make_pattern();
        apply_lifecycle_action(&mut p, &LifecycleAction::Deprecate);
        assert_eq!(p.lifecycle.status, LifecycleStatus::Deprecated);
    }
}
