//! Maturity promotion and demotion for patterns.
//!
//! Maturity levels: Draft → Emerging → Stable → Canonical
//! Promotion requires sustained evidence; demotion triggers on poor effectiveness.

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::knowledge::Maturity;
use mur_common::pattern::Pattern;

use crate::store::yaml::YamlStore;

/// Evaluate whether a pattern's maturity should change.
///
/// Returns `Some(new_maturity)` if a change is warranted, `None` otherwise.
///
/// Promotion rules:
/// - Draft → Emerging: injection_count >= 3 AND override_signals == 0
/// - Emerging → Stable: injection_count >= 10 AND effectiveness >= 0.6 AND age >= 7 days
/// - Stable → Canonical: injection_count >= 30 AND effectiveness >= 0.8 AND age >= 30 days AND pinned
///
/// Demotion rules:
/// - Canonical → Stable: effectiveness < 0.6
/// - Stable → Emerging: effectiveness < 0.4 OR inactive > 2x half_life
/// - Emerging → Draft: effectiveness < 0.2
pub fn evaluate_maturity(pattern: &Pattern, now: DateTime<Utc>) -> Option<Maturity> {
    let current = pattern.maturity;
    let evidence = &pattern.evidence;
    let effectiveness = evidence.effectiveness();
    let age_days = (now - pattern.created_at).num_days();

    let new_maturity = match current {
        Maturity::Draft => {
            if evidence.injection_count >= 3 && evidence.override_signals == 0 {
                Some(Maturity::Emerging)
            } else {
                None
            }
        }
        Maturity::Emerging => {
            // Check demotion first
            if effectiveness < 0.2 {
                Some(Maturity::Draft)
            }
            // Then promotion
            else if evidence.injection_count >= 10 && effectiveness >= 0.6 && age_days >= 7 {
                Some(Maturity::Stable)
            } else {
                None
            }
        }
        Maturity::Stable => {
            // Check demotion first
            if effectiveness < 0.4 {
                return Some(Maturity::Emerging);
            }
            if is_inactive_beyond(pattern, now, 2.0) {
                return Some(Maturity::Emerging);
            }
            // Then promotion
            if evidence.injection_count >= 30
                && effectiveness >= 0.8
                && age_days >= 30
                && pattern.lifecycle.pinned
            {
                Some(Maturity::Canonical)
            } else {
                None
            }
        }
        Maturity::Canonical => {
            if effectiveness < 0.6 {
                Some(Maturity::Stable)
            } else {
                None
            }
        }
    };

    // Only return if actually changed
    new_maturity.filter(|m| *m != current)
}

/// Check if a pattern has been inactive for more than `factor` * half_life days.
fn is_inactive_beyond(pattern: &Pattern, now: DateTime<Utc>, factor: f64) -> bool {
    let half_life = pattern
        .decay
        .half_life_override
        .or(pattern.lifecycle.decay_half_life)
        .unwrap_or_else(|| pattern.tier.decay_half_life_days()) as f64;

    let last_activity = [
        pattern.lifecycle.last_injected,
        pattern.decay.last_active,
        pattern.evidence.last_validated,
    ]
    .iter()
    .filter_map(|d| *d)
    .max()
    .unwrap_or(pattern.created_at);

    let days_inactive = (now - last_activity).num_days().max(0) as f64;
    days_inactive > factor * half_life
}

/// Report from applying maturity evaluation across all patterns.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct MaturityReport {
    pub patterns_scanned: usize,
    pub promotions: usize,
    pub demotions: usize,
    pub details: Vec<MaturityDetail>,
}

/// Detail of a single maturity change.
#[derive(Debug)]
pub struct MaturityDetail {
    pub name: String,
    pub old_maturity: Maturity,
    pub new_maturity: Maturity,
    pub is_promotion: bool,
}

/// Scan all patterns, evaluate maturity, and save changes.
pub fn apply_maturity_all(store: &YamlStore, now: DateTime<Utc>) -> Result<MaturityReport> {
    let patterns = store.list_all()?;
    let mut report = MaturityReport {
        patterns_scanned: patterns.len(),
        ..Default::default()
    };

    for mut p in patterns {
        if let Some(new_maturity) = evaluate_maturity(&p, now) {
            let is_promotion = maturity_rank(new_maturity) > maturity_rank(p.maturity);
            let detail = MaturityDetail {
                name: p.name.clone(),
                old_maturity: p.maturity,
                new_maturity,
                is_promotion,
            };

            if is_promotion {
                report.promotions += 1;
            } else {
                report.demotions += 1;
            }

            p.maturity = new_maturity;
            p.updated_at = now;
            store.save(&p)?;

            report.details.push(detail);
        }
    }

    Ok(report)
}

/// Evaluate maturity in dry-run mode — returns report without saving.
pub fn apply_maturity_all_dry_run(store: &YamlStore, now: DateTime<Utc>) -> Result<MaturityReport> {
    let patterns = store.list_all()?;
    let mut report = MaturityReport {
        patterns_scanned: patterns.len(),
        ..Default::default()
    };

    for p in patterns {
        if let Some(new_maturity) = evaluate_maturity(&p, now) {
            let is_promotion = maturity_rank(new_maturity) > maturity_rank(p.maturity);
            if is_promotion {
                report.promotions += 1;
            } else {
                report.demotions += 1;
            }
            report.details.push(MaturityDetail {
                name: p.name.clone(),
                old_maturity: p.maturity,
                new_maturity,
                is_promotion,
            });
        }
    }

    Ok(report)
}

/// Numeric rank for maturity comparison.
fn maturity_rank(m: Maturity) -> u8 {
    match m {
        Maturity::Draft => 0,
        Maturity::Emerging => 1,
        Maturity::Stable => 2,
        Maturity::Canonical => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::*;
    use tempfile::TempDir;

    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: name.into(),
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
                created_at: Utc::now(),
                updated_at: Utc::now(),
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    // ─── Promotion tests ──────────────────────────────────────────────

    #[test]
    fn test_draft_to_emerging() {
        let now = Utc::now();
        let mut p = make_pattern("draft");
        p.maturity = Maturity::Draft;
        p.evidence.injection_count = 3;
        p.evidence.success_signals = 3;
        p.evidence.override_signals = 0;

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Emerging));
    }

    #[test]
    fn test_draft_no_promotion_with_overrides() {
        let now = Utc::now();
        let mut p = make_pattern("draft-overrides");
        p.maturity = Maturity::Draft;
        p.evidence.injection_count = 5;
        p.evidence.success_signals = 4;
        p.evidence.override_signals = 1; // has overrides

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, None);
    }

    #[test]
    fn test_draft_no_promotion_low_count() {
        let now = Utc::now();
        let mut p = make_pattern("draft-low");
        p.maturity = Maturity::Draft;
        p.evidence.injection_count = 2; // < 3

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, None);
    }

    #[test]
    fn test_emerging_to_stable() {
        let now = Utc::now();
        let mut p = make_pattern("emerging");
        p.maturity = Maturity::Emerging;
        p.evidence.injection_count = 10;
        p.evidence.success_signals = 8;
        p.evidence.override_signals = 2; // 80% effectiveness
        p.created_at = now - Duration::days(10); // > 7 days old

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Stable));
    }

    #[test]
    fn test_emerging_no_promotion_too_young() {
        let now = Utc::now();
        let mut p = make_pattern("young");
        p.maturity = Maturity::Emerging;
        p.evidence.injection_count = 10;
        p.evidence.success_signals = 8;
        p.evidence.override_signals = 2;
        p.created_at = now - Duration::days(3); // < 7 days

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, None);
    }

    #[test]
    fn test_stable_to_canonical() {
        let now = Utc::now();
        let mut p = make_pattern("stable");
        p.maturity = Maturity::Stable;
        p.evidence.injection_count = 30;
        p.evidence.success_signals = 28;
        p.evidence.override_signals = 2; // 93% effectiveness
        p.lifecycle.pinned = true;
        p.lifecycle.last_injected = Some(now); // recent activity
        p.created_at = now - Duration::days(60); // > 30 days old

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Canonical));
    }

    #[test]
    fn test_stable_no_canonical_without_pin() {
        let now = Utc::now();
        let mut p = make_pattern("stable-unpin");
        p.maturity = Maturity::Stable;
        p.evidence.injection_count = 30;
        p.evidence.success_signals = 28;
        p.evidence.override_signals = 2;
        p.lifecycle.pinned = false; // not pinned
        p.lifecycle.last_injected = Some(now);
        p.created_at = now - Duration::days(60);

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, None);
    }

    // ─── Demotion tests ───────────────────────────────────────────────

    #[test]
    fn test_canonical_to_stable() {
        let now = Utc::now();
        let mut p = make_pattern("canonical");
        p.maturity = Maturity::Canonical;
        p.evidence.success_signals = 4;
        p.evidence.override_signals = 6; // 40% < 60%

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Stable));
    }

    #[test]
    fn test_canonical_stays_with_good_effectiveness() {
        let now = Utc::now();
        let mut p = make_pattern("canon-good");
        p.maturity = Maturity::Canonical;
        p.evidence.success_signals = 8;
        p.evidence.override_signals = 2; // 80%

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, None);
    }

    #[test]
    fn test_stable_to_emerging_low_effectiveness() {
        let now = Utc::now();
        let mut p = make_pattern("stable-low");
        p.maturity = Maturity::Stable;
        p.evidence.success_signals = 2;
        p.evidence.override_signals = 8; // 20% < 40%
        p.lifecycle.last_injected = Some(now); // recent, so not inactive

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Emerging));
    }

    #[test]
    fn test_stable_to_emerging_inactive() {
        let now = Utc::now();
        let mut p = make_pattern("stable-inactive");
        p.maturity = Maturity::Stable;
        p.tier = Tier::Session; // 14-day half-life
        p.evidence.success_signals = 5;
        p.evidence.override_signals = 3; // 62.5% > 40% (so not demoted by effectiveness)
        // Inactive for > 2 * 14 = 28 days
        p.created_at = now - Duration::days(50);
        // No activity timestamps → uses created_at which is 50 days ago > 28

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Emerging));
    }

    #[test]
    fn test_emerging_to_draft() {
        let now = Utc::now();
        let mut p = make_pattern("emerging-bad");
        p.maturity = Maturity::Emerging;
        p.evidence.success_signals = 1;
        p.evidence.override_signals = 9; // 10% < 20%

        let result = evaluate_maturity(&p, now);
        assert_eq!(result, Some(Maturity::Draft));
    }

    // ─── apply_maturity_all tests ─────────────────────────────────────

    #[test]
    fn test_apply_maturity_all() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        // Pattern that should promote: Draft → Emerging
        let mut p1 = make_pattern("will-promote");
        p1.maturity = Maturity::Draft;
        p1.evidence.injection_count = 5;
        p1.evidence.success_signals = 5;
        p1.evidence.override_signals = 0;
        store.save(&p1)?;

        // Pattern that should stay Draft
        let mut p2 = make_pattern("stays-draft");
        p2.maturity = Maturity::Draft;
        p2.evidence.injection_count = 1;
        store.save(&p2)?;

        let report = apply_maturity_all(&store, Utc::now())?;
        assert_eq!(report.promotions, 1);
        assert_eq!(report.demotions, 0);
        assert_eq!(report.details.len(), 1);
        assert_eq!(report.details[0].name, "will-promote");

        let loaded = store.get("will-promote")?;
        assert_eq!(loaded.maturity, Maturity::Emerging);

        let loaded2 = store.get("stays-draft")?;
        assert_eq!(loaded2.maturity, Maturity::Draft);

        Ok(())
    }

    #[test]
    fn test_apply_maturity_dry_run() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("dry-run");
        p.maturity = Maturity::Draft;
        p.evidence.injection_count = 5;
        p.evidence.success_signals = 5;
        p.evidence.override_signals = 0;
        store.save(&p)?;

        let report = apply_maturity_all_dry_run(&store, Utc::now())?;
        assert_eq!(report.promotions, 1);

        // Should NOT be modified on disk
        let loaded = store.get("dry-run")?;
        assert_eq!(loaded.maturity, Maturity::Draft);

        Ok(())
    }
}
