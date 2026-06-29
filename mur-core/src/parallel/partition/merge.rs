#![allow(dead_code, unused_imports)]
//! Merge agents' versions of their assigned units back into one file.
//! Reuses `cherry::assemble::assemble_file` for byte-safe splicing.

use std::collections::HashMap;

use anyhow::Result;

use crate::parallel::cherry::{CherryPlan, UnitSelection};
use crate::parallel::cherry::assemble::{TrackSource, assemble_file};
use crate::parallel::semantic::SupportedLanguage;

use super::PartitionPlan;

/// Convert a `PartitionPlan` into the `CherryPlan` that `assemble_file` consumes.
/// Each assigned unit's "winning track" is simply the agent that owns it.
pub fn partition_to_cherry_plan(plan: &PartitionPlan) -> CherryPlan {
    let mut selections = HashMap::new();
    for assignment in &plan.assignments {
        for name in &assignment.unit_names {
            selections.insert(
                name.clone(),
                UnitSelection {
                    unit_name: name.clone(),
                    winning_track: assignment.track_name.clone(),
                    score: 1.0,
                    low_confidence: false,
                },
            );
        }
    }
    CherryPlan { selections }
}

/// Merge each agent's version of its region back into one file.
/// `agent_sources`: `(track_name, source_bytes)` for each participating track.
pub fn merge_partition_file(
    base_source: &[u8],
    plan: &PartitionPlan,
    agent_sources: &[(String, Vec<u8>)],
    lang: SupportedLanguage,
) -> Result<Vec<u8>> {
    let cherry = partition_to_cherry_plan(plan);
    let tracks: Vec<TrackSource<'_>> = agent_sources
        .iter()
        .map(|(name, src)| TrackSource {
            track_name: name.as_str(),
            source: src.as_slice(),
        })
        .collect();
    assemble_file(base_source, &cherry, &tracks, lang)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::partition::{PartitionPlan, RegionAssignment};

    #[test]
    fn cherry_plan_has_one_selection_per_assigned_unit() {
        let plan = PartitionPlan {
            assignments: vec![
                RegionAssignment {
                    track_name: "t0".into(),
                    unit_names: vec!["a".into(), "b".into()],
                },
                RegionAssignment {
                    track_name: "t1".into(),
                    unit_names: vec!["c".into()],
                },
            ],
        };
        let cp = partition_to_cherry_plan(&plan);
        assert_eq!(cp.selections.len(), 3);
        assert_eq!(cp.winning_track_for("a"), Some("t0"));
        assert_eq!(cp.winning_track_for("c"), Some("t1"));
    }

    #[test]
    fn merge_splices_each_agents_region() {
        let base = b"fn alpha() -> i32 { 0 }\nfn beta() -> i32 { 0 }\n";
        let a0 = b"fn alpha() -> i32 { 11 }\nfn beta() -> i32 { 0 }\n";
        let a1 = b"fn alpha() -> i32 { 0 }\nfn beta() -> i32 { 22 }\n";
        let plan = PartitionPlan {
            assignments: vec![
                RegionAssignment {
                    track_name: "t0".into(),
                    unit_names: vec!["alpha".into()],
                },
                RegionAssignment {
                    track_name: "t1".into(),
                    unit_names: vec!["beta".into()],
                },
            ],
        };
        let merged = merge_partition_file(
            base,
            &plan,
            &[("t0".into(), a0.to_vec()), ("t1".into(), a1.to_vec())],
            SupportedLanguage::Rust,
        )
        .unwrap();
        let out = String::from_utf8(merged).unwrap();
        assert!(out.contains("11"), "alpha from t0: {out}");
        assert!(out.contains("22"), "beta from t1: {out}");
        assert!(!out.contains("{ 0 }"), "no stub bodies should remain: {out}");
    }
}
