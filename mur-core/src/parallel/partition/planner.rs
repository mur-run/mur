#![allow(dead_code, unused_imports)]
//! Balanced LPT (Longest-Processing-Time) bin-packing for semantic units.

use std::cmp::Reverse;
use std::collections::HashSet;

use anyhow::{bail, Result};

use crate::parallel::semantic::SemanticUnit;

use super::{PartitionPlan, RegionAssignment};

/// Assign `units` across `track_names` using the greedy LPT heuristic:
/// sort largest first, place each unit on the currently-lightest track.
pub fn plan_partition(units: &[SemanticUnit], track_names: &[String]) -> Result<PartitionPlan> {
    if track_names.is_empty() {
        bail!("partition needs at least one track");
    }
    if units.is_empty() {
        bail!("no semantic units to partition");
    }
    let n = track_names.len();
    let mut sorted: Vec<&SemanticUnit> = units.iter().collect();
    sorted.sort_by_key(|u| Reverse(u.byte_range.end - u.byte_range.start));

    let mut loads = vec![0usize; n];
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); n];
    for u in sorted {
        // index of the lightest track (ties → lowest index for determinism)
        let idx = loads
            .iter()
            .enumerate()
            .min_by_key(|(i, l)| (*l, *i))
            .map(|(i, _)| i)
            .expect("n >= 1");
        loads[idx] += u.byte_range.end - u.byte_range.start;
        buckets[idx].push(u.name.clone());
    }

    let assignments = track_names
        .iter()
        .zip(buckets)
        .map(|(name, unit_names)| RegionAssignment {
            track_name: name.clone(),
            unit_names,
        })
        .collect();
    Ok(PartitionPlan { assignments })
}

/// Verify every unit is covered exactly once (disjoint + complete).
pub fn validate_coverage(plan: &PartitionPlan, units: &[SemanticUnit]) -> Result<()> {
    let mut seen: HashSet<&str> = HashSet::new();
    for a in &plan.assignments {
        for name in &a.unit_names {
            if !seen.insert(name.as_str()) {
                bail!("unit `{name}` assigned to more than one track");
            }
        }
    }
    for u in units {
        if !seen.contains(u.name.as_str()) {
            bail!("unit `{}` not assigned to any track", u.name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::semantic::UnitKind;

    fn unit(name: &str, size: usize) -> SemanticUnit {
        SemanticUnit {
            kind: UnitKind::Fn,
            name: name.into(),
            byte_range: 0..size,
            line_range: 0..1,
            content_hash: [0u8; 32],
            dependencies: vec![],
        }
    }

    #[test]
    fn every_unit_assigned_exactly_once() {
        let units = vec![unit("a", 10), unit("b", 20), unit("c", 5), unit("d", 8)];
        let tracks = vec!["t0".to_string(), "t1".to_string()];
        let plan = plan_partition(&units, &tracks).unwrap();
        validate_coverage(&plan, &units).unwrap();
        let total: usize = plan.assignments.iter().map(|a| a.unit_names.len()).sum();
        assert_eq!(total, 4);
        assert_eq!(plan.assignments.len(), 2);
    }

    #[test]
    fn lpt_balances_by_size() {
        // Sizes 8,7,6,5,4 across 2 tracks.
        let units = vec![
            unit("a", 8),
            unit("b", 7),
            unit("c", 6),
            unit("d", 5),
            unit("e", 4),
        ];
        let tracks = vec!["t0".to_string(), "t1".to_string()];
        let plan = plan_partition(&units, &tracks).unwrap();
        let load = |names: &[String]| -> usize {
            names
                .iter()
                .map(|n| {
                    units
                        .iter()
                        .find(|u| &u.name == n)
                        .unwrap()
                        .byte_range
                        .len()
                })
                .sum()
        };
        let l0 = load(&plan.assignments[0].unit_names);
        let l1 = load(&plan.assignments[1].unit_names);
        assert!(
            (l0 as i64 - l1 as i64).abs() <= 8,
            "imbalanced: {l0} vs {l1}"
        );
    }

    #[test]
    fn zero_tracks_errors() {
        let units = vec![unit("a", 10)];
        assert!(plan_partition(&units, &[]).is_err());
    }

    #[test]
    fn duplicate_detection_in_validate() {
        let units = vec![unit("a", 10)];
        let plan = PartitionPlan {
            assignments: vec![
                RegionAssignment {
                    track_name: "t0".into(),
                    unit_names: vec!["a".into()],
                },
                RegionAssignment {
                    track_name: "t1".into(),
                    unit_names: vec!["a".into()],
                },
            ],
        };
        assert!(validate_coverage(&plan, &units).is_err());
    }
}
