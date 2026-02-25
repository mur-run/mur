//! Time-based confidence decay for patterns.
//!
//! Confidence decays exponentially: `confidence * 0.5^(days_inactive / half_life)`
//! where `days_inactive` is measured from the most recent activity timestamp.

use anyhow::Result;
use chrono::{DateTime, Utc};
use mur_common::pattern::{LifecycleStatus, Pattern};

use crate::store::yaml::YamlStore;

/// Calculate the decayed confidence for a pattern at a given time.
///
/// Formula: `confidence * 0.5^(days_inactive / half_life)`
/// - `days_inactive` = days since max(last_injected, last_active, last_validated)
/// - `half_life` = pattern.decay.half_life_override OR lifecycle.decay_half_life OR tier default
pub fn calculate_decay(pattern: &Pattern, now: DateTime<Utc>) -> f64 {
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

    let half_life = pattern
        .decay
        .half_life_override
        .or(pattern.lifecycle.decay_half_life)
        .unwrap_or_else(|| pattern.tier.decay_half_life_days()) as f64;

    if half_life <= 0.0 {
        return pattern.confidence;
    }

    pattern.confidence * 0.5_f64.powf(days_inactive / half_life)
}

/// Report from applying decay across all patterns.
#[derive(Debug, Default)]
pub struct DecayReport {
    pub patterns_scanned: usize,
    pub patterns_decayed: usize,
    pub patterns_archived: usize,
    pub details: Vec<DecayDetail>,
}

/// Detail of a single pattern's decay change.
#[derive(Debug)]
pub struct DecayDetail {
    pub name: String,
    pub old_confidence: f64,
    pub new_confidence: f64,
    pub auto_archived: bool,
}

/// Scan all patterns, apply decay, and save changes.
///
/// - Pinned patterns skip decay
/// - Muted patterns skip decay
/// - Already-archived patterns skip decay
/// - If confidence drops below 0.1 → auto-archive
pub fn apply_decay_all(store: &YamlStore, now: DateTime<Utc>) -> Result<DecayReport> {
    let patterns = store.list_all()?;
    let mut report = DecayReport {
        patterns_scanned: patterns.len(),
        ..Default::default()
    };

    for mut p in patterns {
        // Skip: pinned, muted, already archived
        if p.lifecycle.pinned || p.lifecycle.muted {
            continue;
        }
        if p.lifecycle.status == LifecycleStatus::Archived {
            continue;
        }

        let old_confidence = p.confidence;
        let new_confidence = calculate_decay(&p, now);

        // Skip if no meaningful change (< 0.001)
        if (old_confidence - new_confidence).abs() < 0.001 {
            continue;
        }

        p.confidence = new_confidence;
        let mut auto_archived = false;

        if new_confidence < 0.1 {
            p.lifecycle.status = LifecycleStatus::Archived;
            auto_archived = true;
            report.patterns_archived += 1;
        }

        p.updated_at = now;
        store.save(&p)?;

        report.patterns_decayed += 1;
        report.details.push(DecayDetail {
            name: p.name.clone(),
            old_confidence,
            new_confidence,
            auto_archived,
        });
    }

    Ok(report)
}

/// Apply decay in dry-run mode — returns the report without saving.
pub fn apply_decay_all_dry_run(store: &YamlStore, now: DateTime<Utc>) -> Result<DecayReport> {
    let patterns = store.list_all()?;
    let mut report = DecayReport {
        patterns_scanned: patterns.len(),
        ..Default::default()
    };

    for p in patterns {
        if p.lifecycle.pinned || p.lifecycle.muted {
            continue;
        }
        if p.lifecycle.status == LifecycleStatus::Archived {
            continue;
        }

        let old_confidence = p.confidence;
        let new_confidence = calculate_decay(&p, now);

        if (old_confidence - new_confidence).abs() < 0.001 {
            continue;
        }

        let auto_archived = new_confidence < 0.1;
        if auto_archived {
            report.patterns_archived += 1;
        }

        report.patterns_decayed += 1;
        report.details.push(DecayDetail {
            name: p.name.clone(),
            old_confidence,
            new_confidence,
            auto_archived,
        });
    }

    Ok(report)
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
                confidence: 1.0,
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
    fn test_zero_days_no_decay() {
        let mut p = make_pattern("fresh");
        p.decay.last_active = Some(Utc::now());
        let decayed = calculate_decay(&p, Utc::now());
        assert!(
            (decayed - p.confidence).abs() < 0.001,
            "0 days inactive should produce no decay"
        );
    }

    #[test]
    fn test_half_life_days_50_percent() {
        let now = Utc::now();
        let mut p = make_pattern("half");
        p.confidence = 1.0;
        p.tier = Tier::Session; // 14-day half-life
        p.decay.last_active = Some(now - Duration::days(14));

        let decayed = calculate_decay(&p, now);
        assert!(
            (decayed - 0.5).abs() < 0.01,
            "After exactly 1 half-life, confidence should be ~0.5, got {}",
            decayed
        );
    }

    #[test]
    fn test_two_half_lives_25_percent() {
        let now = Utc::now();
        let mut p = make_pattern("two-hl");
        p.confidence = 1.0;
        p.tier = Tier::Session; // 14-day half-life
        p.decay.last_active = Some(now - Duration::days(28));

        let decayed = calculate_decay(&p, now);
        assert!(
            (decayed - 0.25).abs() < 0.01,
            "After 2 half-lives, confidence should be ~0.25, got {}",
            decayed
        );
    }

    #[test]
    fn test_project_tier_slower_decay() {
        let now = Utc::now();
        let mut p = make_pattern("project");
        p.confidence = 1.0;
        p.tier = Tier::Project; // 90-day half-life
        p.decay.last_active = Some(now - Duration::days(14));

        let decayed = calculate_decay(&p, now);
        // 14 days with 90-day half-life: 0.5^(14/90) ≈ 0.897
        assert!(
            decayed > 0.85,
            "Project tier should decay slowly at 14 days, got {}",
            decayed
        );
    }

    #[test]
    fn test_core_tier_very_slow_decay() {
        let now = Utc::now();
        let mut p = make_pattern("core");
        p.confidence = 1.0;
        p.tier = Tier::Core; // 365-day half-life
        p.decay.last_active = Some(now - Duration::days(14));

        let decayed = calculate_decay(&p, now);
        // 14 days with 365-day half-life: very minor
        assert!(
            decayed > 0.97,
            "Core tier should barely decay at 14 days, got {}",
            decayed
        );
    }

    #[test]
    fn test_half_life_override() {
        let now = Utc::now();
        let mut p = make_pattern("override");
        p.confidence = 1.0;
        p.tier = Tier::Session; // would be 14 days
        p.decay.half_life_override = Some(7); // override to 7 days
        p.decay.last_active = Some(now - Duration::days(7));

        let decayed = calculate_decay(&p, now);
        assert!(
            (decayed - 0.5).abs() < 0.01,
            "With 7-day override half-life after 7 days, should be ~0.5, got {}",
            decayed
        );
    }

    #[test]
    fn test_uses_most_recent_activity() {
        let now = Utc::now();
        let mut p = make_pattern("recent");
        p.confidence = 1.0;
        p.tier = Tier::Session;
        // Old last_active but very recent last_injected
        p.decay.last_active = Some(now - Duration::days(100));
        p.lifecycle.last_injected = Some(now - Duration::days(1));

        let decayed = calculate_decay(&p, now);
        // Should use last_injected (1 day ago), not last_active (100 days ago)
        assert!(
            decayed > 0.95,
            "Should use most recent activity timestamp, got {}",
            decayed
        );
    }

    #[test]
    fn test_falls_back_to_created_at() {
        let now = Utc::now();
        let mut p = make_pattern("fallback");
        p.confidence = 1.0;
        p.tier = Tier::Session;
        p.created_at = now - Duration::days(14);
        // No activity timestamps at all

        let decayed = calculate_decay(&p, now);
        assert!(
            (decayed - 0.5).abs() < 0.01,
            "Should fall back to created_at, got {}",
            decayed
        );
    }

    #[test]
    fn test_pinned_patterns_skip_decay() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("pinned");
        p.lifecycle.pinned = true;
        p.created_at = Utc::now() - Duration::days(100);
        store.save(&p)?;

        let now = Utc::now();
        let report = apply_decay_all(&store, now)?;
        assert_eq!(
            report.patterns_decayed, 0,
            "Pinned patterns should skip decay"
        );

        let loaded = store.get("pinned")?;
        assert!(
            (loaded.confidence - 1.0).abs() < 0.001,
            "Pinned pattern confidence should be unchanged"
        );

        Ok(())
    }

    #[test]
    fn test_muted_patterns_skip_decay() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("muted");
        p.lifecycle.muted = true;
        p.created_at = Utc::now() - Duration::days(100);
        store.save(&p)?;

        let report = apply_decay_all(&store, Utc::now())?;
        assert_eq!(
            report.patterns_decayed, 0,
            "Muted patterns should skip decay"
        );

        Ok(())
    }

    #[test]
    fn test_auto_archive_below_threshold() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("low-conf");
        p.confidence = 0.15; // will decay below 0.1
        p.tier = Tier::Session; // 14-day half-life
        p.created_at = Utc::now() - Duration::days(14);
        // No activity timestamps → uses created_at
        store.save(&p)?;

        let report = apply_decay_all(&store, Utc::now())?;
        assert_eq!(report.patterns_archived, 1, "Should auto-archive");

        let loaded = store.get("low-conf")?;
        assert_eq!(loaded.lifecycle.status, LifecycleStatus::Archived);

        Ok(())
    }

    #[test]
    fn test_apply_decay_all_basic() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("decaying");
        p.confidence = 1.0;
        p.tier = Tier::Session;
        p.created_at = Utc::now() - Duration::days(14);
        store.save(&p)?;

        let report = apply_decay_all(&store, Utc::now())?;
        assert_eq!(report.patterns_decayed, 1);
        assert_eq!(report.patterns_scanned, 1);

        let loaded = store.get("decaying")?;
        assert!(
            (loaded.confidence - 0.5).abs() < 0.05,
            "Should have decayed to ~0.5, got {}",
            loaded.confidence
        );

        Ok(())
    }

    #[test]
    fn test_dry_run_does_not_modify() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("immutable");
        p.confidence = 1.0;
        p.tier = Tier::Session;
        p.created_at = Utc::now() - Duration::days(14);
        store.save(&p)?;

        let report = apply_decay_all_dry_run(&store, Utc::now())?;
        assert_eq!(report.patterns_decayed, 1);

        // Pattern should be unchanged on disk
        let loaded = store.get("immutable")?;
        assert!(
            (loaded.confidence - 1.0).abs() < 0.001,
            "Dry run should not modify files, confidence = {}",
            loaded.confidence
        );

        Ok(())
    }
}
