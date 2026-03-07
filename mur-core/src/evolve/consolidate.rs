//! Memory consolidation — orchestrates dedup, contradiction detection,
//! promotion, decay, and archival in a single pass.

use anyhow::Result;
use chrono::Utc;
use mur_common::pattern::{LifecycleStatus, Pattern};
use serde::Serialize;

use crate::store::yaml::YamlStore;

use super::decay;
use super::lifecycle::{self, LifecycleAction};
use super::maturity;

/// Report from a consolidation run.
#[derive(Debug, Default, Serialize)]
pub struct ConsolidationReport {
    pub patterns_scanned: usize,
    pub duplicates_merged: usize,
    pub contradictions_resolved: usize,
    pub promotions: usize,
    pub maturity_promotions: usize,
    pub maturity_demotions: usize,
    pub patterns_decayed: usize,
    pub patterns_archived: usize,
    pub details: Vec<ConsolidationDetail>,
}

/// A single consolidation action taken.
#[derive(Debug, Serialize)]
pub struct ConsolidationDetail {
    pub pattern_name: String,
    pub action: String,
    pub detail: String,
}

/// Run full consolidation pipeline on all patterns.
pub fn consolidate(store: &YamlStore, dry_run: bool) -> Result<ConsolidationReport> {
    let patterns = store.list_all()?;
    let mut report = ConsolidationReport {
        patterns_scanned: patterns.len(),
        ..Default::default()
    };

    // Phase 1: Dedup — find patterns with very similar names/descriptions
    dedup_pass(&patterns, store, dry_run, &mut report)?;

    // Phase 2: Contradiction detection
    contradiction_pass(&patterns, store, dry_run, &mut report)?;

    // Phase 3: Auto-promotion (session→project)
    let patterns = store.list_all()?; // reload after dedup
    promotion_pass(&patterns, store, dry_run, &mut report)?;

    // Phase 3b: Maturity evaluation (Draft→Emerging→Stable→Canonical)
    let now = Utc::now();
    let maturity_report = if dry_run {
        maturity::apply_maturity_all_dry_run(store, now)?
    } else {
        maturity::apply_maturity_all(store, now)?
    };
    report.maturity_promotions += maturity_report.promotions;
    report.maturity_demotions += maturity_report.demotions;
    for d in &maturity_report.details {
        let action_str = if d.is_promotion {
            "maturity-promoted"
        } else {
            "maturity-demoted"
        };
        report.details.push(ConsolidationDetail {
            pattern_name: d.name.clone(),
            action: action_str.into(),
            detail: format!("{:?} → {:?}", d.old_maturity, d.new_maturity),
        });
    }

    // Phase 4: Decay + archival
    if dry_run {
        let decay_report = decay::apply_decay_all_dry_run(store, now)?;
        report.patterns_decayed += decay_report.patterns_decayed;
        report.patterns_archived += decay_report.patterns_archived;
        for d in &decay_report.details {
            report.details.push(ConsolidationDetail {
                pattern_name: d.name.clone(),
                action: if d.auto_archived {
                    "archived".into()
                } else {
                    "decayed".into()
                },
                detail: format!(
                    "confidence {:.2} → {:.2}",
                    d.old_confidence, d.new_confidence
                ),
            });
        }
    } else {
        let decay_report = decay::apply_decay_all(store, now)?;
        report.patterns_decayed += decay_report.patterns_decayed;
        report.patterns_archived += decay_report.patterns_archived;
        for d in &decay_report.details {
            report.details.push(ConsolidationDetail {
                pattern_name: d.name.clone(),
                action: if d.auto_archived {
                    "archived".into()
                } else {
                    "decayed".into()
                },
                detail: format!(
                    "confidence {:.2} → {:.2}",
                    d.old_confidence, d.new_confidence
                ),
            });
        }
    }

    // Phase 5: Stale archival (deprecated > 180 days)
    let patterns = store.list_all()?;
    archival_pass(&patterns, store, dry_run, &mut report)?;

    Ok(report)
}

/// Find and merge near-duplicate patterns by keyword overlap.
/// Similarity threshold for deduplication.
/// Raised from 0.80 → 0.85 to reduce false-positive merges.
const DEDUP_SIMILARITY_THRESHOLD: f64 = 0.85;

fn dedup_pass(
    patterns: &[Pattern],
    store: &YamlStore,
    dry_run: bool,
    report: &mut ConsolidationReport,
) -> Result<()> {
    let mut merged_names: Vec<String> = Vec::new();

    for i in 0..patterns.len() {
        if merged_names.contains(&patterns[i].name) {
            continue;
        }
        for j in (i + 1)..patterns.len() {
            if merged_names.contains(&patterns[j].name) {
                continue;
            }
            let sim = keyword_similarity(&patterns[i], &patterns[j]);
            if sim >= DEDUP_SIMILARITY_THRESHOLD {
                // Keep the higher-scoring pattern; the other becomes the loser.
                let (keeper, loser) = if score_pattern(&patterns[i]) >= score_pattern(&patterns[j])
                {
                    (&patterns[i], &patterns[j])
                } else {
                    (&patterns[j], &patterns[i])
                };

                report.details.push(ConsolidationDetail {
                    pattern_name: loser.name.clone(),
                    action: "merged".into(),
                    detail: format!(
                        "duplicate of '{}' (similarity {:.0}%)",
                        keeper.name,
                        sim * 100.0
                    ),
                });
                report.duplicates_merged += 1;
                merged_names.push(loser.name.clone());

                if !dry_run {
                    // Merge evidence from the loser into the keeper before archiving.
                    // This preserves injection history and success signals rather
                    // than silently discarding them.
                    let mut keeper_mut = store.get(&keeper.name)?;
                    let loser_loaded = store.get(&loser.name)?;
                    keeper_mut.evidence.injection_count += loser_loaded.evidence.injection_count;
                    keeper_mut.evidence.success_signals += loser_loaded.evidence.success_signals;
                    keeper_mut.evidence.override_signals += loser_loaded.evidence.override_signals;
                    // Merge tags: add any topics from the loser that the keeper lacks.
                    for tag in &loser_loaded.tags.topics {
                        if !keeper_mut.tags.topics.contains(tag) {
                            keeper_mut.tags.topics.push(tag.clone());
                        }
                    }
                    // Link keeper → loser so the relationship is traceable.
                    if !keeper_mut.links.related.contains(&loser.name) {
                        keeper_mut.links.related.push(loser.name.clone());
                    }
                    keeper_mut.updated_at = Utc::now();
                    store.save(&keeper_mut)?;

                    // Archive the loser.
                    let mut loser_mut = loser_loaded;
                    loser_mut.lifecycle.status = LifecycleStatus::Archived;
                    loser_mut.updated_at = Utc::now();
                    store.save(&loser_mut)?;
                }
            }
        }
    }
    Ok(())
}

/// Detect contradictions: patterns on the same topic with opposing content.
///
/// Safety constraints (both must hold before we consider two patterns contradictory):
/// 1. **Shared topic tag**: at least one topic tag in common.  If either pattern
///    has no tags the pair is skipped entirely — keyword negation alone is too
///    broad to be reliable without topical grounding.
/// 2. **Negation keyword pair**: the contents contain an opposing pair such as
///    "always" / "never".
///
/// When a contradiction is detected we only *deprecate* the lower-confidence
/// pattern (we never delete or archive during contradiction resolution).
fn contradiction_pass(
    patterns: &[Pattern],
    store: &YamlStore,
    dry_run: bool,
    report: &mut ConsolidationReport,
) -> Result<()> {
    let negation_pairs = [
        ("always", "never"),
        ("use ", "avoid "),
        ("prefer", "don't prefer"),
        ("enable", "disable"),
    ];

    for i in 0..patterns.len() {
        if patterns[i].lifecycle.status != LifecycleStatus::Active {
            continue;
        }
        let tags_i: Vec<&str> = patterns[i].tags.topics.iter().map(|s| s.as_str()).collect();

        // Safety: skip contradiction check entirely if this pattern has no tags.
        if tags_i.is_empty() {
            continue;
        }

        let content_i = patterns[i].content.as_text().to_lowercase();

        for j in (i + 1)..patterns.len() {
            if patterns[j].lifecycle.status != LifecycleStatus::Active {
                continue;
            }
            let tags_j: Vec<&str> = patterns[j].tags.topics.iter().map(|s| s.as_str()).collect();

            // Safety: skip if the other pattern also has no tags.
            if tags_j.is_empty() {
                continue;
            }

            // Require at least one shared topic tag to anchor the contradiction.
            let shared_tag = tags_i.iter().any(|t| tags_j.contains(t));
            if !shared_tag {
                continue;
            }

            let content_j = patterns[j].content.as_text().to_lowercase();

            // Check for negation pairs.
            let contradicts = negation_pairs.iter().any(|(pos, neg)| {
                (content_i.contains(pos) && content_j.contains(neg))
                    || (content_i.contains(neg) && content_j.contains(pos))
            });

            if contradicts {
                let (winner, loser) = if patterns[i].confidence >= patterns[j].confidence {
                    (&patterns[i], &patterns[j])
                } else {
                    (&patterns[j], &patterns[i])
                };

                report.details.push(ConsolidationDetail {
                    pattern_name: loser.name.clone(),
                    action: "contradiction".into(),
                    detail: format!(
                        "contradicts '{}' on shared tags [{}]; deprecated (lower confidence)",
                        winner.name,
                        tags_i
                            .iter()
                            .filter(|t| tags_j.contains(*t))
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                });
                report.contradictions_resolved += 1;

                if !dry_run {
                    let mut loser_mut = store.get(&loser.name)?;
                    loser_mut.lifecycle.status = LifecycleStatus::Deprecated;
                    loser_mut.updated_at = Utc::now();
                    store.save(&loser_mut)?;
                }
            }
        }
    }
    Ok(())
}

/// Auto-promote patterns that meet promotion criteria.
fn promotion_pass(
    patterns: &[Pattern],
    store: &YamlStore,
    dry_run: bool,
    report: &mut ConsolidationReport,
) -> Result<()> {
    for p in patterns {
        let action = lifecycle::evaluate_lifecycle(p);
        if let LifecycleAction::Promote(tier) = &action {
            report.details.push(ConsolidationDetail {
                pattern_name: p.name.clone(),
                action: "promoted".into(),
                detail: format!("{:?} → {:?}", p.tier, tier),
            });
            report.promotions += 1;

            if !dry_run {
                let mut p_mut = store.get(&p.name)?;
                lifecycle::apply_lifecycle_action(&mut p_mut, &action);
                store.save(&p_mut)?;
            }
        }
    }
    Ok(())
}

/// Archive deprecated patterns older than 180 days.
fn archival_pass(
    patterns: &[Pattern],
    store: &YamlStore,
    dry_run: bool,
    report: &mut ConsolidationReport,
) -> Result<()> {
    for p in patterns {
        if p.lifecycle.status != LifecycleStatus::Deprecated {
            continue;
        }
        let action = lifecycle::evaluate_lifecycle(p);
        if action == LifecycleAction::Archive {
            report.details.push(ConsolidationDetail {
                pattern_name: p.name.clone(),
                action: "archived".into(),
                detail: "deprecated > 180 days".into(),
            });
            report.patterns_archived += 1;

            if !dry_run {
                let mut p_mut = store.get(&p.name)?;
                lifecycle::apply_lifecycle_action(&mut p_mut, &action);
                store.save(&p_mut)?;
            }
        }
    }
    Ok(())
}

/// Compute keyword similarity between two patterns (Jaccard on words).
fn keyword_similarity(a: &Pattern, b: &Pattern) -> f64 {
    let words_a: std::collections::HashSet<String> =
        format!("{} {} {}", a.name, a.description, a.content.as_text())
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .filter(|w| w.len() > 2)
            .collect();

    let words_b: std::collections::HashSet<String> =
        format!("{} {} {}", b.name, b.description, b.content.as_text())
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .filter(|w| w.len() > 2)
            .collect();

    if words_a.is_empty() || words_b.is_empty() {
        return 0.0;
    }

    let intersection = words_a.intersection(&words_b).count() as f64;
    let union = words_a.union(&words_b).count() as f64;
    intersection / union
}

/// Score a pattern for dedup comparison (higher = keep).
fn score_pattern(p: &Pattern) -> f64 {
    p.confidence * 0.4 + p.importance * 0.3 + p.evidence.effectiveness() * 0.3
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::{Content, Evidence, Tags, Tier};
    use tempfile::TempDir;

    fn make_pattern(name: &str, desc: &str, content: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: name.into(),
                description: desc.into(),
                content: Content::Plain(content.into()),
                tier: Tier::Session,
                importance: 0.5,
                confidence: 0.7,
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
    fn test_consolidate_empty_store() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;
        let report = consolidate(&store, false)?;
        assert_eq!(report.patterns_scanned, 0);
        Ok(())
    }

    #[test]
    fn test_dedup_merges_similar() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        // Use identical description+content with a large shared vocabulary so
        // Jaccard ≥ 0.85 even accounting for the differing name tokens.
        // Shared word bag must dominate the name difference: 20+ shared words,
        // 2 unique (the two name tokens) → Jaccard ≥ 20/22 ≈ 0.91.
        let long_content = "Use the @Test macro attribute instead of XCTest class subclassing \
            for all Swift unit integration end-to-end acceptance tests in your project";
        let mut p1 = make_pattern("swift-test-macro-a", long_content, long_content);
        p1.base.evidence.injection_count = 10;
        p1.base.evidence.success_signals = 7;

        let mut p2 = make_pattern("swift-test-macro-b", long_content, long_content);
        p2.base.confidence = 0.5;
        p2.base.evidence.injection_count = 3;
        p2.base.evidence.success_signals = 2;

        store.save(&p1)?;
        store.save(&p2)?;

        let report = consolidate(&store, false)?;
        assert!(
            report.duplicates_merged >= 1,
            "Should merge similar patterns"
        );

        // Loser must be archived
        let p2_loaded = store.get("swift-test-macro-b")?;
        assert_eq!(p2_loaded.lifecycle.status, LifecycleStatus::Archived);

        // Keeper must have accumulated evidence from both patterns
        let p1_loaded = store.get("swift-test-macro-a")?;
        assert_eq!(
            p1_loaded.evidence.injection_count, 13,
            "Keeper should accumulate injection count from loser"
        );
        assert_eq!(
            p1_loaded.evidence.success_signals, 9,
            "Keeper should accumulate success signals from loser"
        );

        // Keeper must link to loser
        assert!(
            p1_loaded
                .links
                .related
                .contains(&"swift-test-macro-b".to_string()),
            "Keeper must link to the archived loser"
        );
        Ok(())
    }

    #[test]
    fn test_dedup_below_threshold_not_merged() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        // These are similar but below the 0.85 threshold
        let p1 = make_pattern(
            "rust-errors",
            "Rust error handling",
            "Use anyhow for errors",
        );
        let p2 = make_pattern(
            "go-errors",
            "Go error handling",
            "Use fmt.Errorf for errors",
        );

        store.save(&p1)?;
        store.save(&p2)?;

        let report = consolidate(&store, false)?;
        assert_eq!(
            report.duplicates_merged, 0,
            "Dissimilar patterns must not be merged"
        );
        Ok(())
    }

    #[test]
    fn test_promotion_threshold() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("promotable", "Good pattern", "Content");
        p.base.evidence = Evidence {
            injection_count: 10,
            success_signals: 8,
            override_signals: 1,
            ..Default::default()
        };
        store.save(&p)?;

        let report = consolidate(&store, false)?;
        assert!(report.promotions >= 1, "Should promote pattern");

        let loaded = store.get("promotable")?;
        assert_eq!(loaded.tier, Tier::Project);
        Ok(())
    }

    #[test]
    fn test_dry_run_no_changes() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p = make_pattern("promotable", "Good pattern", "Content");
        p.base.evidence = Evidence {
            injection_count: 10,
            success_signals: 8,
            override_signals: 1,
            ..Default::default()
        };
        store.save(&p)?;

        let report = consolidate(&store, true)?;
        assert!(report.promotions >= 1);

        let loaded = store.get("promotable")?;
        assert_eq!(loaded.tier, Tier::Session);
        Ok(())
    }

    #[test]
    fn test_contradiction_detection() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        let mut p1 = make_pattern("use-tabs", "Use tabs", "Always use tabs for indentation");
        p1.base.tags = Tags {
            topics: vec!["formatting".into()],
            ..Default::default()
        };
        p1.base.confidence = 0.9;

        let mut p2 = make_pattern("never-tabs", "Never tabs", "Never use tabs for indentation");
        p2.base.tags = Tags {
            topics: vec!["formatting".into()],
            ..Default::default()
        };
        p2.base.confidence = 0.5;

        store.save(&p1)?;
        store.save(&p2)?;

        let report = consolidate(&store, false)?;
        assert!(
            report.contradictions_resolved >= 1,
            "Should detect contradiction"
        );

        let p2_loaded = store.get("never-tabs")?;
        assert_eq!(p2_loaded.lifecycle.status, LifecycleStatus::Deprecated);
        Ok(())
    }

    #[test]
    fn test_contradiction_untagged_patterns_not_flagged() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        // Both patterns have opposing content but NO tags → must NOT be flagged.
        let p1 = make_pattern("always-tabs", "Use tabs", "Always use tabs for indentation");
        // no tags set — uses default
        let p2 = make_pattern(
            "never-spaces",
            "No spaces",
            "Never use spaces for indentation",
        );
        // no tags

        store.save(&p1)?;
        store.save(&p2)?;

        let report = consolidate(&store, false)?;
        assert_eq!(
            report.contradictions_resolved, 0,
            "Untagged patterns must not trigger contradiction detection"
        );
        Ok(())
    }

    #[test]
    fn test_contradiction_different_topics_not_flagged() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = YamlStore::new(tmp.path().to_path_buf())?;

        // Opposing words but completely different topics → must NOT be flagged.
        let mut p1 = make_pattern(
            "use-dark-mode",
            "Dark mode",
            "Always use dark mode for the UI",
        );
        p1.base.tags = Tags {
            topics: vec!["ui".into()],
            ..Default::default()
        };

        let mut p2 = make_pattern(
            "never-skip-tests",
            "No test skipping",
            "Never skip tests before deploying",
        );
        p2.base.tags = Tags {
            topics: vec!["testing".into()],
            ..Default::default()
        };

        store.save(&p1)?;
        store.save(&p2)?;

        let report = consolidate(&store, false)?;
        assert_eq!(
            report.contradictions_resolved, 0,
            "Cross-topic patterns with no shared tags must not be marked as contradictions"
        );
        Ok(())
    }
}
