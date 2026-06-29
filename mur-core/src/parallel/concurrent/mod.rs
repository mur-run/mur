//! Concurrent N-way merge for parallel-track worktrees (Model A, post-hoc).
//! Default OFF — requires MUR_PARALLEL_CONCURRENT=1.
//! Guarantees deterministic order-independent convergence of merged bytes, NOT correctness.

pub mod hunk;
pub mod stats;
pub mod structural;

/// Identifies which agent/track produced an edit. Uses track name as the id
/// so tie-breaks produce byte-stable output.
pub type ActorId = String;

/// A region of `base` where two or more actors made conflicting edits.
/// NEVER auto-merged; callers must escalate (judge/cherry/human).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlapRegion {
    pub base_line_range: std::ops::Range<u32>,
    pub actor_ids: Vec<ActorId>,
}

impl std::fmt::Display for OverlapRegion {
    /// Formats as `lines N–M (actors: a, b)` where N and M are the inclusive
    /// start and exclusive end of the conflicting base-line range.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lines {}–{} (actors: {})",
            self.base_line_range.start,
            self.base_line_range.end,
            self.actor_ids.join(", "),
        )
    }
}

/// Result of merging N versions against a common base.
#[derive(Debug, Clone)]
pub struct MergeOutcome {
    pub merged: Vec<u8>,
    pub overlaps: Vec<OverlapRegion>,
}

impl MergeOutcome {
    pub fn is_clean(&self) -> bool {
        self.overlaps.is_empty()
    }

    /// Returns a one-line human-readable summary: `"N clean, M overlaps"`.
    ///
    /// `N` is the number of lines in the merged output that were not part of a
    /// conflicting region (i.e. total merged lines minus lines spanned by overlap
    /// regions in base coordinates — a useful lower-bound proxy when the merge is
    /// partial).  `M` is the number of escalated [`OverlapRegion`]s.
    pub fn summary(&self) -> String {
        let total_lines = self
            .merged
            .split(|&b| b == b'\n')
            // A trailing newline produces a final empty slice; don't count it.
            .filter(|s| !s.is_empty())
            .count();
        let conflict_lines: usize = self
            .overlaps
            .iter()
            .map(|r| (r.base_line_range.end - r.base_line_range.start) as usize)
            .sum();
        let clean = total_lines.saturating_sub(conflict_lines);
        format!("{} clean, {} overlaps", clean, self.overlaps.len())
    }
}

/// Merge N independently-edited versions of one file against a common base.
pub trait ConcurrentMerger {
    fn merge(&self, base: &[u8], versions: &[(ActorId, Vec<u8>)]) -> anyhow::Result<MergeOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_outcome_is_clean_when_no_overlaps() {
        let o = MergeOutcome { merged: b"x".to_vec(), overlaps: vec![] };
        assert!(o.is_clean());
        let o2 = MergeOutcome {
            merged: b"x".to_vec(),
            overlaps: vec![OverlapRegion { base_line_range: 0..1, actor_ids: vec!["a".into()] }],
        };
        assert!(!o2.is_clean());
    }

    // ── Display impl for OverlapRegion ────────────────────────────────────────

    #[test]
    fn overlap_region_display_single_actor() {
        let r = OverlapRegion { base_line_range: 3..7, actor_ids: vec!["alice".into()] };
        assert_eq!(r.to_string(), "lines 3–7 (actors: alice)");
    }

    #[test]
    fn overlap_region_display_multiple_actors() {
        let r = OverlapRegion {
            base_line_range: 10..12,
            actor_ids: vec!["alice".into(), "bob".into()],
        };
        assert_eq!(r.to_string(), "lines 10–12 (actors: alice, bob)");
    }

    // ── summary() on MergeOutcome ─────────────────────────────────────────────

    #[test]
    fn summary_clean_merge() {
        // Three-line merged output, no conflicts.
        let merged = b"line1\nline2\nline3\n".to_vec();
        let o = MergeOutcome { merged, overlaps: vec![] };
        // 3 lines total, 0 conflict lines → "3 clean, 0 overlaps"
        assert_eq!(o.summary(), "3 clean, 0 overlaps");
    }

    #[test]
    fn summary_with_overlaps() {
        // Five-line merged output (base content kept in place for conflict),
        // one OverlapRegion spanning 2 base lines.
        let merged = b"a\nb\nc\nd\ne\n".to_vec();
        let o = MergeOutcome {
            merged,
            overlaps: vec![OverlapRegion {
                base_line_range: 1..3, // 2 conflict lines
                actor_ids: vec!["x".into(), "y".into()],
            }],
        };
        // 5 total − 2 conflict = 3 clean, 1 overlap
        assert_eq!(o.summary(), "3 clean, 1 overlaps");
    }
}
