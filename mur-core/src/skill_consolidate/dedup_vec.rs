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
    report.duplicates.sort_by(|a, b| {
        a.a.cmp(&b.a).then_with(|| a.b.cmp(&b.b))
    });
}
