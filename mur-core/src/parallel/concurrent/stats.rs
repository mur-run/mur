//! Spike-1 overlap-rate instrumentation: decides whether Loro CRDT engine is worth building.

use anyhow::Result;
use serde::Serialize;

use super::ConcurrentMerger;
use super::hunk::{Edit, Group, group_edits, hunks_vs_base};

/// Accumulated statistics from one or more concurrent merges.
#[derive(Debug, Default, Serialize)]
pub struct OverlapStats {
    pub n_tracks: usize,
    pub files_compared: usize,
    pub clean_groups: usize,
    pub overlap_regions: usize,
    /// Fraction of hunk groups that had at least one overlap. Populated by `finalize`.
    pub overlap_rate: f64,
}

impl OverlapStats {
    /// Compute `overlap_rate` from accumulated counts.
    pub fn finalize(&mut self) {
        let total = self.clean_groups + self.overlap_regions;
        self.overlap_rate = if total > 0 {
            self.overlap_regions as f64 / total as f64
        } else {
            0.0
        };
    }
}

/// Count `(clean_groups, overlap_regions)` for one merge operation.
/// Uses hunk grouping directly; `_merger` param reserved for future typing.
pub fn count_groups(
    _merger: &impl ConcurrentMerger,
    base: &[u8],
    versions: &[(String, Vec<u8>)],
) -> Result<(usize, usize)> {
    let base_str = std::str::from_utf8(base)?;
    let edits: Vec<Edit> = versions
        .iter()
        .flat_map(|(actor, ver)| {
            let ver_str = std::str::from_utf8(ver).unwrap_or_default();
            hunks_vs_base(base_str, ver_str)
                .into_iter()
                .map(move |h| Edit {
                    actor: actor.clone(),
                    hunk: h,
                })
        })
        .collect();
    let groups = group_edits(edits);
    let clean = groups
        .iter()
        .filter(|g| matches!(g, Group::Clean { .. }))
        .count();
    let overlaps = groups
        .iter()
        .filter(|g| matches!(g, Group::Conflict { .. }))
        .count();
    Ok((clean, overlaps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::concurrent::structural::StructuralMerger;

    fn vers(pairs: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        pairs
            .iter()
            .map(|(a, s)| (a.to_string(), s.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn counts_clean_and_overlap_groups() {
        let base = b"a\nb\nc\n";
        // x edits line 0 (clean), x & y both edit line 2 divergently (overlap)
        let v = vers(&[("x", "X\nb\nC1\n"), ("y", "a\nb\nC2\n")]);
        let (clean, overlaps) = count_groups(&StructuralMerger, base, &v).unwrap();
        assert_eq!(clean, 1);
        assert_eq!(overlaps, 1);
    }

    #[test]
    fn overlap_rate_finalizes() {
        let mut s = OverlapStats {
            clean_groups: 3,
            overlap_regions: 1,
            ..Default::default()
        };
        s.finalize();
        assert!((s.overlap_rate - 0.25).abs() < 1e-9);
    }

    #[test]
    fn empty_run_zero_rate() {
        let mut s = OverlapStats::default();
        s.finalize();
        assert_eq!(s.overlap_rate, 0.0);
    }
}
