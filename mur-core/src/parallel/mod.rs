//! Speculative parallel agent execution — P0/P1 orchestrator.
#![allow(dead_code, unused_imports)]

pub mod backend;
pub mod cherry;
pub mod judge;
pub mod semantic;
pub mod state;
pub mod track;

use anyhow::Result;
use mur_common::{config::BackendConfig, parallel::ParallelConfig};
use semantic::{SupportedLanguage, cas::group_by_identity, extract_units};
use state::ParallelStateDb;
use std::sync::Arc;
use track::TrackSet;
use walkdir::WalkDir;

use crate::conversations::backend::factory::build_for_stage;
use judge::{CyclicJudge, JudgeTask, TrackImpl};
use track::filter::{FilterResult, run_pre_filter};

/// Run the full judge pipeline over all tracks:
/// pre-filter → semantic parse → CAS dedup → LLM judge (cached) → LMDB store.
///
/// Async because `CyclicJudge::score()` calls an LLM. Sync callers:
/// `tokio::runtime::Runtime::new()?.block_on(run_judge_pipeline_async(...))`.
pub async fn run_judge_pipeline_async(
    tracks: &TrackSet,
    config: &ParallelConfig,
    state_db: &ParallelStateDb,
) -> Result<()> {
    // 1. Pre-filter: discard tracks that fail cargo check / clippy.
    let live_tracks: Vec<&track::Track> = tracks
        .tracks
        .iter()
        .filter(|t| {
            let res = run_pre_filter(&t.worktree_path, &config.pre_filter);
            if matches!(res, FilterResult::Failed { .. }) {
                eprintln!(" track {} failed pre-filter — discarded", t.config.name);
                false
            } else {
                true
            }
        })
        .collect();

    if live_tracks.is_empty() {
        eprintln!("all tracks failed pre-filter — nothing to judge");
        return Ok(());
    }

    // 2. Parse .rs files in each surviving track; keep unit + its source bytes.
    type TrackUnits = Vec<(String, Vec<(semantic::SemanticUnit, Vec<u8>)>)>;
    let mut per_track: TrackUnits = Vec::new();
    for t in &live_tracks {
        let mut units_with_src: Vec<(semantic::SemanticUnit, Vec<u8>)> = Vec::new();
        for entry in WalkDir::new(&t.worktree_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            if let Ok(source) = std::fs::read(entry.path())
                && let Ok(units) = extract_units(&source, SupportedLanguage::Rust)
            {
                for u in units {
                    let unit_src = source[u.byte_range.clone()].to_vec();
                    units_with_src.push((u, unit_src));
                }
            }
        }
        per_track.push((t.config.name.clone(), units_with_src));
    }

    // 3. CAS dedup: group units by name+hash.
    let for_grouping: Vec<(&str, Vec<semantic::SemanticUnit>)> = per_track
        .iter()
        .map(|(name, uws)| (name.as_str(), uws.iter().map(|(u, _)| u.clone()).collect()))
        .collect();
    let groups = group_by_identity(&for_grouping);

    if groups.needs_judge.is_empty() {
        eprintln!("all units identical across tracks (CAS hit) — no LLM calls needed");
        return Ok(());
    }

    // 4. Build LLM backend + judge once.
    let backend_cfg = BackendConfig {
        provider: "anthropic".into(),
        model: config.judge.model.clone(),
        endpoint: None,
        api_key_env: None,
        timeout_secs: Some(120),
    };
    let backend = build_for_stage(&backend_cfg, "parallel.judge")?;
    let judge = CyclicJudge::new(config.judge.clone(), Arc::clone(&backend));

    let rubric_version = config.judge.rubric.version();
    let mut cache_hits = 0usize;
    let mut judge_calls = 0usize;

    // 5. Judge each differing unit, using LMDB cache by content hash.
    for jg in &groups.needs_judge {
        let mut impls: Vec<TrackImpl> = Vec::new();
        for (track_name, unit) in &jg.per_track {
            let unit_src = per_track
                .iter()
                .find(|(tn, _)| tn == track_name)
                .and_then(|(_, uws)| {
                    uws.iter()
                        .find(|(u, _)| u.name == unit.name && u.content_hash == unit.content_hash)
                })
                .map(|(_, src)| src.clone())
                .unwrap_or_default();

            let track_config = tracks
                .tracks
                .iter()
                .find(|t| &t.config.name == track_name)
                .map(|t| t.config.clone())
                .unwrap_or_else(|| mur_common::parallel::TrackConfig {
                    name: track_name.clone(),
                    approach: String::new(),
                    model: None,
                });

            impls.push(TrackImpl {
                track_config,
                unit: unit.clone(),
                source: unit_src,
            });
        }

        if impls.is_empty() {
            continue;
        }

        // Cache check: use first impl's hash as group representative.
        if state_db
            .get_score(&impls[0].unit.content_hash, &rubric_version)?
            .is_some()
        {
            cache_hits += 1;
            continue;
        }

        let task = JudgeTask {
            unit_name: jg.name.clone(),
            implementations: impls,
            rubric_version: rubric_version.clone(),
        };
        let scores = judge.score(&task).await?;
        judge_calls += 1;

        for score in &scores {
            if let Some(imp) = task
                .implementations
                .iter()
                .find(|i| i.track_config.name == score.track_name)
            {
                let js = state::JudgeScore {
                    score: score.score,
                    reasoning: score.reasoning.clone(),
                    model: config.judge.model.clone(),
                    ts: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                };
                state_db.put_score(&imp.unit.content_hash, &rubric_version, &js)?;
            }
        }
    }

    eprintln!(
        "judge complete: {} units, {} cache hits, {} LLM calls",
        groups.needs_judge.len(),
        cache_hits,
        judge_calls
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::{JudgeConfig, ParallelConfig, Rubric};

    #[test]
    fn pipeline_smoke_with_empty_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let state_db = state::ParallelStateDb::open(dir.path()).unwrap();
        let config = ParallelConfig {
            mode: Default::default(),
            tracks: vec![],
            judge: JudgeConfig {
                model: "claude-haiku-4-5".into(),
                rubric: Rubric::default(),
            },
            pre_filter: vec![],
        };
        let tracks = TrackSet { tracks: vec![] };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(run_judge_pipeline_async(&tracks, &config, &state_db));
        assert!(result.is_ok());
    }

    #[test]
    fn gate0_tree_sitter_extraction_on_real_source() {
        // A6 validation: tree-sitter reliably extracts semantic units from real mur-core source.
        let filter_src = include_bytes!("../parallel/track/filter.rs");
        let units = extract_units(filter_src, SupportedLanguage::Rust).unwrap();
        assert!(
            units.len() >= 2,
            "expected ≥2 semantic units in filter.rs, got {}",
            units.len()
        );
        for u in &units {
            assert!(!u.name.is_empty(), "unit has empty name");
            assert!(
                u.byte_range.end > u.byte_range.start,
                "unit {} has empty byte range",
                u.name
            );
            assert_ne!(u.content_hash, [0u8; 32], "unit {} has zero hash", u.name);
        }
    }
}
