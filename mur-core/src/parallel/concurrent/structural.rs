//! Zero-dependency `ConcurrentMerger`: disjoint hunks auto-merge, overlapping regions escalate.

use anyhow::Context as _;

use super::{ConcurrentMerger, MergeOutcome, OverlapRegion};
use super::hunk::{Edit, Group, group_edits, hunks_vs_base};

/// Merges N versions by line-hunk grouping.
/// Disjoint and byte-identical hunks are applied; overlapping divergent hunks are escalated
/// (base content preserved in place, region reported in `MergeOutcome::overlaps`).
pub struct StructuralMerger;

impl ConcurrentMerger for StructuralMerger {
    fn merge(&self, base: &[u8], versions: &[(String, Vec<u8>)]) -> anyhow::Result<MergeOutcome> {
        let base_str = std::str::from_utf8(base).context("base is not valid UTF-8")?;

        let mut edits: Vec<Edit> = Vec::new();
        for (actor, ver) in versions {
            let ver_str = std::str::from_utf8(ver).context("version is not valid UTF-8")?;
            for h in hunks_vs_base(base_str, ver_str) {
                edits.push(Edit { actor: actor.clone(), hunk: h });
            }
        }

        let groups = group_edits(edits);

        let mut clean = Vec::new();
        let mut overlaps: Vec<OverlapRegion> = Vec::new();

        for g in groups {
            match g {
                Group::Clean { hunk, .. } => clean.push(hunk),
                Group::Conflict { base_start, base_end, actors } => {
                    overlaps.push(OverlapRegion { base_line_range: base_start..base_end, actor_ids: actors });
                }
            }
        }

        // Apply clean hunks from high base_start to low so earlier indices remain valid.
        // Within the same start, apply larger ranges first (replacements before zero-width insertions).
        clean.sort_by_key(|h| (std::cmp::Reverse(h.base_start), std::cmp::Reverse(h.base_end)));

        let mut out_lines: Vec<String> = base_str.lines().map(|s| s.to_string()).collect();
        for h in clean {
            let start = h.base_start as usize;
            let end = h.base_end as usize;
            out_lines.splice(start..end, h.replacement.into_iter());
        }

        let mut merged = out_lines.join("\n");
        if base_str.ends_with('\n') {
            merged.push('\n');
        }

        Ok(MergeOutcome { merged: merged.into_bytes(), overlaps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vers(pairs: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        pairs.iter().map(|(a, s)| (a.to_string(), s.as_bytes().to_vec())).collect()
    }

    #[test]
    fn disjoint_edits_both_applied() {
        let base = b"a\nb\nc\n";
        let out = StructuralMerger
            .merge(base, &vers(&[("x", "A\nb\nc\n"), ("y", "a\nb\nC\n")]))
            .unwrap();
        assert!(out.is_clean());
        assert_eq!(String::from_utf8(out.merged).unwrap(), "A\nb\nC\n");
    }

    #[test]
    fn overlapping_edits_escalate() {
        let base = b"a\nb\nc\n";
        let v = vers(&[("x", "a\nFROM_X\nc\n"), ("y", "a\nFROM_Y\nc\n")]);
        let out = StructuralMerger.merge(base, &v).unwrap();
        assert!(!out.is_clean());
        assert_eq!(out.overlaps.len(), 1);
        assert_eq!(out.overlaps[0].base_line_range, 1..2);
        // conflicting region keeps base content (no silent interleave)
        assert_eq!(String::from_utf8(out.merged).unwrap(), "a\nb\nc\n");
    }

    #[test]
    fn order_independent_output() {
        let base = b"a\nb\nc\n";
        let out1 = StructuralMerger
            .merge(base, &vers(&[("x", "A\nb\nc\n"), ("y", "a\nb\nC\n")]))
            .unwrap();
        let out2 = StructuralMerger
            .merge(base, &vers(&[("y", "a\nb\nC\n"), ("x", "A\nb\nc\n")]))
            .unwrap();
        assert_eq!(out1.merged, out2.merged);
    }

    #[test]
    fn identical_edits_apply_once() {
        let base = b"a\nb\nc\n";
        let v = vers(&[("x", "a\nB\nc\n"), ("y", "a\nB\nc\n")]);
        let out = StructuralMerger.merge(base, &v).unwrap();
        assert!(out.is_clean());
        assert_eq!(String::from_utf8(out.merged).unwrap(), "a\nB\nc\n");
    }

    #[test]
    fn non_utf8_errors() {
        let base = &[0xff, 0xfe][..];
        let v = vec![("x".to_string(), vec![0x00])];
        assert!(StructuralMerger.merge(base, &v).is_err());
    }
}
