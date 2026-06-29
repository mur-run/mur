//! Blake3 content-addressed storage (CAS) for semantic units.
//!
//! Identifies which parallel-track implementations are identical (skip LLM judging)
//! vs different (need judging).

use super::SemanticUnit;
#[cfg(test)]
use super::UnitKind;

/// Compare two semantic units by their content hash.
pub fn units_differ(a: &SemanticUnit, b: &SemanticUnit) -> bool {
    a.content_hash != b.content_hash
}

/// A semantic unit that is identical across all tracks (no judging needed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipUnit {
    pub name: String,
    pub content_hash: [u8; 32],
}

/// A group of semantic units with the same name but possibly different content.
#[derive(Debug, Clone)]
pub struct JudgeGroup {
    pub name: String,
    /// (track_name, unit) ordered by track index
    pub per_track: Vec<(String, SemanticUnit)>,
}

/// Result of grouping semantic units by identity.
#[derive(Debug, Clone)]
pub struct UnitGroups {
    pub skip: Vec<SkipUnit>,
    pub needs_judge: Vec<JudgeGroup>,
}

/// Group semantic units by identity across tracks.
///
/// Units with the same name and identical content hashes across all tracks that have them
/// are placed in `skip`. Units with the same name but differing content hashes are placed
/// in `needs_judge`.
///
/// A unit may not appear in all tracks — it appears in `per_track` for the tracks that have it.
pub fn group_by_identity(tracks: &[(&str, Vec<SemanticUnit>)]) -> UnitGroups {
    // Collect all unit names across all tracks
    let mut all_names: Vec<String> = tracks
        .iter()
        .flat_map(|(_, units)| units.iter().map(|u| u.name.clone()))
        .collect();
    all_names.sort();
    all_names.dedup();

    let mut skip = Vec::new();
    let mut needs_judge = Vec::new();

    for name in all_names {
        let per_track: Vec<(String, SemanticUnit)> = tracks
            .iter()
            .filter_map(|(track_name, units)| {
                units
                    .iter()
                    .find(|u| u.name == name)
                    .map(|u| (track_name.to_string(), u.clone()))
            })
            .collect();

        // Collect unique hashes
        let mut hashes: Vec<[u8; 32]> = per_track.iter().map(|(_, u)| u.content_hash).collect();
        hashes.sort();
        hashes.dedup();

        if hashes.len() == 1 {
            skip.push(SkipUnit {
                name,
                content_hash: hashes[0],
            });
        } else {
            needs_judge.push(JudgeGroup { name, per_track });
        }
    }

    UnitGroups { skip, needs_judge }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_unit(name: &str, hash: [u8; 32]) -> SemanticUnit {
        SemanticUnit {
            kind: UnitKind::Fn,
            name: name.to_string(),
            byte_range: 0..10,
            line_range: 0..1,
            content_hash: hash,
            dependencies: vec![],
        }
    }

    #[test]
    fn same_hash_not_different() {
        let hash = [1u8; 32];
        assert!(!units_differ(
            &make_unit("f", hash),
            &make_unit("f", hash)
        ));
    }

    #[test]
    fn different_hash_is_different() {
        let a = make_unit("f", [1u8; 32]);
        let b = make_unit("f", [2u8; 32]);
        assert!(units_differ(&a, &b));
    }

    #[test]
    fn groups_identical_units_as_no_judge_needed() {
        let hash = [42u8; 32];
        let tracks = vec![
            ("track-a", vec![make_unit("authenticate", hash)]),
            ("track-b", vec![make_unit("authenticate", hash)]),
        ];
        let groups = group_by_identity(&tracks);
        assert!(
            groups.needs_judge.is_empty(),
            "identical units should not need judging"
        );
        assert_eq!(groups.skip.len(), 1);
        assert_eq!(groups.skip[0].name, "authenticate");
        assert_eq!(groups.skip[0].content_hash, hash);
    }

    #[test]
    fn groups_different_units_for_judging() {
        let hash_a = [1u8; 32];
        let hash_b = [2u8; 32];
        let tracks = vec![
            ("track-a", vec![make_unit("process", hash_a)]),
            ("track-b", vec![make_unit("process", hash_b)]),
        ];
        let groups = group_by_identity(&tracks);
        assert!(groups.skip.is_empty(), "different units should not be skipped");
        assert_eq!(groups.needs_judge.len(), 1);
        assert_eq!(groups.needs_judge[0].name, "process");
        assert_eq!(groups.needs_judge[0].per_track.len(), 2);
        assert_eq!(groups.needs_judge[0].per_track[0].0, "track-a");
        assert_eq!(groups.needs_judge[0].per_track[1].0, "track-b");
        assert!(units_differ(
            &groups.needs_judge[0].per_track[0].1,
            &groups.needs_judge[0].per_track[1].1
        ));
    }
}
