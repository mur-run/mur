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
}
