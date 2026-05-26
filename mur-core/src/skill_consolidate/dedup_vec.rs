use std::collections::HashMap;

use super::dedup::{DedupSource, DuplicatePair, KeeperReason};
use super::{ConsolidateReport, SkillView};
use crate::skill_index::SKILL_SOURCE_ID;
use crate::store::embedding::{self, EmbeddingConfig};
use crate::store::vector::{SearchFilter, VectorStore};

/// Cosine-similarity threshold for "very similar but not identical" skills.
///
/// 0.92 matches sentence-transformer "near-duplicate" band — high enough to
/// avoid flagging loose conceptual overlap (e.g., two skills both about
/// "search" but with different operational intent), yet low enough to catch
/// paraphrased duplicates. This is embedder-model-dependent; if the embedder
/// is swapped, this threshold may need re-tuning.
pub const COSINE_THRESHOLD: f32 = 0.92;
pub const TOP_K: usize = 8;

pub async fn scan(
    skills: &[SkillView],
    config: &EmbeddingConfig,
    store: &dyn VectorStore,
    report: &mut ConsolidateReport,
) -> anyhow::Result<()> {
    let filter = SearchFilter {
        source_ids: Some(vec![SKILL_SOURCE_ID.into()]),
        since: None,
    };

    for s in skills {
        let q = embedding::embed(&s.embed_text, config).await?;
        let hits = store.search(&q, TOP_K, &filter).await?;
        for hit in hits {
            if hit.external_id == s.name {
                continue;
            }
            if hit.score < COSINE_THRESHOLD {
                continue;
            }

            // Order pair lexicographically to make symmetric matches deterministic.
            let (a_name, b_name) = if s.name < hit.external_id {
                (s.name.clone(), hit.external_id)
            } else {
                (hit.external_id, s.name.clone())
            };

            // Find the SkillViews for keeper selection.
            let view_a = skills.iter().find(|v| v.name == a_name);
            let view_b = skills.iter().find(|v| v.name == b_name);
            let (keeper, _loser, _reason) = match (view_a, view_b) {
                (Some(a), Some(b)) => super::dedup::select_keeper(a, b),
                (Some(a), None) => (a.name.clone(), b_name.clone(), KeeperReason::Alphabetical),
                (None, Some(b)) => (b.name.clone(), a_name.clone(), KeeperReason::Alphabetical),
                (None, None) => continue,
            };

            report.duplicates.push(DuplicatePair {
                a: a_name,
                b: b_name,
                similarity: hit.score as f64,
                keeper,
                kept_reason: _reason,
                source: DedupSource::Vector,
            });
        }
    }
    dedup_combined(report);
    Ok(())
}

/// Walk emitted pairs; collapse (a,b) appearing under both Jaccard and Vector
/// into a single entry with `source = Both`. Preserves the higher similarity.
fn dedup_combined(report: &mut ConsolidateReport) {
    let mut by_pair: HashMap<(String, String), DuplicatePair> = HashMap::new();
    for p in report.duplicates.drain(..) {
        let key = if p.a < p.b {
            (p.a.clone(), p.b.clone())
        } else {
            (p.b.clone(), p.a.clone())
        };
        by_pair
            .entry(key)
            .and_modify(|existing| {
                if existing.source != p.source {
                    existing.source = DedupSource::Both;
                }
                if p.similarity > existing.similarity {
                    existing.similarity = p.similarity;
                }
            })
            .or_insert(p);
    }
    report.duplicates = by_pair.into_values().collect();
    report
        .duplicates
        .sort_by(|a, b| a.a.cmp(&b.a).then_with(|| a.b.cmp(&b.b)));
}

#[cfg(test)]
mod tests {
    use super::super::ConsolidateReport;
    use super::super::dedup::{DedupSource, DuplicatePair, KeeperReason};
    use super::*;

    fn duplicate(a: &str, b: &str, sim: f64, source: DedupSource) -> DuplicatePair {
        DuplicatePair {
            a: a.into(),
            b: b.into(),
            similarity: sim,
            keeper: a.into(),
            kept_reason: KeeperReason::Alphabetical,
            source,
        }
    }

    #[test]
    fn dedup_combined_same_pair_becomes_both() {
        let mut report = ConsolidateReport {
            method: super::super::ConsolidateMethod::Both,
            duplicates: vec![
                duplicate("a", "b", 0.85, DedupSource::Jaccard),
                duplicate("a", "b", 0.94, DedupSource::Vector),
            ],
            contradictions: vec![],
            orphans: vec![],
        };
        dedup_combined(&mut report);
        assert_eq!(report.duplicates.len(), 1);
        let p = &report.duplicates[0];
        assert_eq!(p.a, "a");
        assert_eq!(p.b, "b");
        assert_eq!(p.source, DedupSource::Both);
        // Higher similarity (Vector: 0.94) wins.
        assert!((p.similarity - 0.94).abs() < 0.001);
    }

    #[test]
    fn dedup_combined_keeps_higher_similarity() {
        let mut report = ConsolidateReport {
            method: super::super::ConsolidateMethod::Both,
            duplicates: vec![
                duplicate("x", "y", 0.99, DedupSource::Jaccard),
                duplicate("x", "y", 0.80, DedupSource::Vector),
            ],
            contradictions: vec![],
            orphans: vec![],
        };
        dedup_combined(&mut report);
        assert_eq!(report.duplicates.len(), 1);
        assert!((report.duplicates[0].similarity - 0.99).abs() < 0.001);
        assert_eq!(report.duplicates[0].source, DedupSource::Both);
    }

    #[test]
    fn dedup_combined_disjoint_pairs_untouched() {
        let mut report = ConsolidateReport {
            method: super::super::ConsolidateMethod::Both,
            duplicates: vec![
                duplicate("a", "b", 0.90, DedupSource::Jaccard),
                duplicate("c", "d", 0.93, DedupSource::Vector),
            ],
            contradictions: vec![],
            orphans: vec![],
        };
        dedup_combined(&mut report);
        assert_eq!(report.duplicates.len(), 2);
        // Sorted by (a,b)
        assert_eq!(report.duplicates[0].a, "a");
        assert_eq!(report.duplicates[0].source, DedupSource::Jaccard);
        assert_eq!(report.duplicates[1].a, "c");
        assert_eq!(report.duplicates[1].source, DedupSource::Vector);
    }

    #[test]
    fn dedup_combined_idempotent() {
        let mut report = ConsolidateReport {
            method: super::super::ConsolidateMethod::Both,
            duplicates: vec![
                duplicate("a", "b", 0.88, DedupSource::Jaccard),
                duplicate("a", "b", 0.91, DedupSource::Vector),
            ],
            contradictions: vec![],
            orphans: vec![],
        };
        dedup_combined(&mut report);
        let first = report.duplicates.clone();
        dedup_combined(&mut report);
        assert_eq!(report.duplicates.len(), first.len());
        assert_eq!(report.duplicates[0].source, first[0].source);
        assert!((report.duplicates[0].similarity - first[0].similarity).abs() < 0.001);
    }

    #[test]
    fn dedup_combined_empty_report() {
        let mut report = ConsolidateReport {
            method: super::super::ConsolidateMethod::Jaccard,
            duplicates: vec![],
            contradictions: vec![],
            orphans: vec![],
        };
        dedup_combined(&mut report);
        assert!(report.duplicates.is_empty());
    }
}
