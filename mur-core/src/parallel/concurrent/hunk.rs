#![allow(dead_code)]
//! Line-hunk extraction (vs common base) and overlap classification.

/// A contiguous edit relative to `base`: replaces base lines `[base_start, base_end)`
/// with `replacement`. A pure insertion has `base_start == base_end`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub base_start: u32,
    pub base_end: u32,
    pub replacement: Vec<String>,
}

/// A hunk tagged with the actor (track) that produced it.
#[derive(Debug, Clone)]
pub struct Edit {
    pub actor: String,
    pub hunk: Hunk,
}

/// A cluster of edits touching overlapping base ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Group {
    /// All edits in the cluster are byte-identical — safe to apply once.
    Clean { hunk: Hunk, actors: Vec<String> },
    /// Edits in the cluster diverge — must be escalated.
    Conflict {
        base_start: u32,
        base_end: u32,
        actors: Vec<String>,
    },
}

/// Extract line-level hunks between `base` and `version`.
/// Uses `diff::lines` (splits on `str::lines()` so indices align with `base.lines()`).
pub fn hunks_vs_base(base: &str, version: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut base_idx: u32 = 0;
    let mut cur: Option<Hunk> = None;
    for d in diff::lines(base, version) {
        match d {
            diff::Result::Both(_, _) => {
                if let Some(h) = cur.take() {
                    hunks.push(h);
                }
                base_idx += 1;
            }
            diff::Result::Left(_) => {
                let h = cur.get_or_insert(Hunk {
                    base_start: base_idx,
                    base_end: base_idx,
                    replacement: Vec::new(),
                });
                h.base_end = base_idx + 1;
                base_idx += 1;
            }
            diff::Result::Right(r) => {
                let h = cur.get_or_insert(Hunk {
                    base_start: base_idx,
                    base_end: base_idx,
                    replacement: Vec::new(),
                });
                h.replacement.push(r.to_string());
            }
        }
    }
    if let Some(h) = cur.take() {
        hunks.push(h);
    }
    hunks
}

/// True if two hunks touch overlapping base ranges.
/// Two pure insertions overlap only at the same position.
/// A boundary-touching insertion and a replacement are treated as independent.
fn overlaps(a: &Hunk, b: &Hunk) -> bool {
    let a_ins = a.base_start == a.base_end;
    let b_ins = b.base_start == b.base_end;
    if a_ins && b_ins {
        a.base_start == b.base_start
    } else {
        a.base_start < b.base_end && b.base_start < a.base_end
    }
}

/// Cluster edits by overlapping base ranges, then classify each cluster.
/// Edits must already be sorted by `(base_start, base_end)` — this function sorts them.
pub fn group_edits(mut edits: Vec<Edit>) -> Vec<Group> {
    edits.sort_by_key(|e| (e.hunk.base_start, e.hunk.base_end));
    let mut groups: Vec<Group> = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        let mut cluster: Vec<Edit> = vec![edits[i].clone()];
        let mut j = i + 1;
        while j < edits.len() && cluster.iter().any(|c| overlaps(&c.hunk, &edits[j].hunk)) {
            cluster.push(edits[j].clone());
            j += 1;
        }
        i = j;

        let first = &cluster[0].hunk;
        let all_identical = cluster.iter().all(|c| &c.hunk == first);
        let actors: Vec<String> = cluster.iter().map(|c| c.actor.clone()).collect();
        if all_identical {
            groups.push(Group::Clean {
                hunk: first.clone(),
                actors,
            });
        } else {
            let base_start = cluster.iter().map(|c| c.hunk.base_start).min().unwrap();
            let base_end = cluster.iter().map(|c| c.hunk.base_end).max().unwrap();
            groups.push(Group::Conflict {
                base_start,
                base_end,
                actors,
            });
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(base_start: u32, base_end: u32, replacement: &[&str]) -> Hunk {
        Hunk {
            base_start,
            base_end,
            replacement: replacement.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_change_produces_no_hunks() {
        assert_eq!(hunks_vs_base("a\nb\n", "a\nb\n"), vec![]);
    }

    #[test]
    fn replacement_hunk_covers_changed_line() {
        let base = "a\nb\nc\n";
        let ver = "a\nB\nc\n";
        let hunks = hunks_vs_base(base, ver);
        assert_eq!(hunks, vec![h(1, 2, &["B"])]);
    }

    #[test]
    fn pure_insertion_is_zero_width() {
        let base = "a\nc\n";
        let ver = "a\nb\nc\n";
        let hunks = hunks_vs_base(base, ver);
        assert_eq!(hunks, vec![h(1, 1, &["b"])]);
    }

    #[test]
    fn deletion_hunk_has_empty_replacement() {
        let base = "a\nb\nc\n";
        let ver = "a\nc\n";
        let hunks = hunks_vs_base(base, ver);
        assert_eq!(hunks, vec![h(1, 2, &[])]);
    }

    #[test]
    fn disjoint_edits_form_two_clean_groups() {
        let edits = vec![
            Edit {
                actor: "x".into(),
                hunk: h(0, 1, &["X"]),
            },
            Edit {
                actor: "y".into(),
                hunk: h(2, 3, &["Y"]),
            },
        ];
        let groups = group_edits(edits);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| matches!(g, Group::Clean { .. })));
    }

    #[test]
    fn identical_edits_collapse_to_one_clean_group() {
        let edits = vec![
            Edit {
                actor: "x".into(),
                hunk: h(1, 2, &["SAME"]),
            },
            Edit {
                actor: "y".into(),
                hunk: h(1, 2, &["SAME"]),
            },
        ];
        let groups = group_edits(edits);
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            Group::Clean { hunk, actors } => {
                assert_eq!(hunk, &h(1, 2, &["SAME"]));
                assert_eq!(actors.len(), 2);
            }
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn divergent_overlapping_edits_form_conflict() {
        let edits = vec![
            Edit {
                actor: "x".into(),
                hunk: h(1, 2, &["FROM_X"]),
            },
            Edit {
                actor: "y".into(),
                hunk: h(1, 2, &["FROM_Y"]),
            },
        ];
        let groups = group_edits(edits);
        assert_eq!(groups.len(), 1);
        assert!(matches!(groups[0], Group::Conflict { .. }));
    }
}
