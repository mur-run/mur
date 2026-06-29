# Parallel Tracks P3 Phase 0 — Concurrent Merge (zero-dep) + Spike-1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a zero-dependency `ConcurrentMerger` that auto-merges *disjoint* line-hunks from N agent worktrees and *escalates every overlap*, exposed as `mur fleet merge-concurrent`, plus the Spike-1 overlap-rate instrumentation that decides whether the CRDT (Loro) engine is ever worth adding.

**Architecture:** Model A (post-hoc, isolated agents). A new `parallel::concurrent` module: a `ConcurrentMerger` trait, a zero-dep `StructuralMerger` built on the already-present `diff` crate (line-hunk extraction → overlap classification → reverse-order splice of clean hunks), and an `OverlapStats` collector (Spike-1). A flag-gated `mur fleet merge-concurrent` CLI consumes it, reusing the existing cherry promote/cargo-check machinery. **The Loro engine is explicitly NOT in this plan** — it is a separate conditional Phase 1, written only if Spike-1 shows overlap is common.

**Tech Stack:** Rust edition 2024; `diff = "0.1"` (already a direct mur-core dep); `git2` (already present); `serde`/`serde_json` (present). **Zero new crate dependencies.**

## Global Constraints

- Rust edition 2024; `let`-chains stable.
- **Zero new crate dependencies.** Use the existing `diff` crate for line diffing; do NOT add `similar`, `imara-diff`, Loro, or any CRDT crate in this plan.
- The merge guarantees **deterministic, order-independent convergence of merged bytes — NOT correctness**. Never write "correct merge" in code comments, output strings, or docs. Acceptable phrasing: "deterministic order-independent convergence".
- **Default off.** The command refuses unless `MUR_PARALLEL_CONCURRENT=1` is set.
- **Safe gate policy (from spec §7):** auto-accept only when (a) all hunks are disjoint AND (b) `cargo check` passes. ANY overlap → refuse to promote and report it for escalation. Never silently merge overlapping edits.
- Source files are UTF-8 with `\n` line endings; bail on non-UTF-8 input. (Document; do not handle `\r\n` in v1.)
- Deterministic ActorId = the track name (fixed per track) so output is byte-stable.
- Files ≤ 800 lines.
- Brand: user-visible strings use uppercase "MUR"; `mur` lowercase only for the binary/identifiers/paths.
- Reuse, do not reimplement: `cmd::fleet::cherry_cmd::{promote_cherry_result, project_root_from_worktree}`, `parallel::track::TrackSet`, `parallel::backend` git helpers.

---

## File Map

| File | Change |
|---|---|
| `mur-core/src/parallel/concurrent/mod.rs` | **Create** — `ConcurrentMerger` trait, `MergeOutcome`, `OverlapRegion`, `ActorId` |
| `mur-core/src/parallel/concurrent/hunk.rs` | **Create** — line-hunk extraction (`hunks_vs_base`) + overlap grouping (`group_edits`) |
| `mur-core/src/parallel/concurrent/structural.rs` | **Create** — `StructuralMerger` (zero-dep `ConcurrentMerger` impl) |
| `mur-core/src/parallel/concurrent/stats.rs` | **Create** — `OverlapStats` (Spike-1 instrumentation) |
| `mur-core/src/parallel/mod.rs` | **Modify** — add `pub mod concurrent;` |
| `mur-core/src/cmd/fleet/concurrent_cmd.rs` | **Create** — `cmd_fleet_merge_concurrent` (flag-gated; `--stats`/`--promote`/`--target`) |
| `mur-core/src/cmd/fleet/cherry_cmd.rs` | **Modify** — `pub(super)` on `promote_cherry_result` + `project_root_from_worktree` |
| `mur-core/src/cmd/fleet/mod.rs` | **Modify** — `pub mod concurrent_cmd;` |
| `mur-core/src/cli/actions.rs` | **Modify** — `FleetAction::MergeConcurrent { name, stats, promote, target }` |
| `mur-core/src/dispatch.rs` | **Modify** — dispatch arm |
| `docs/superpowers/validation/spike1-overlap-rate.md` | **Create** — Spike-1 how-to-run + results template + decision gate |
| `scripts/gate7_concurrent_merge.sh` | **Create** — round-trip gate (no agents) |
| `CLAUDE.md` | **Modify** — fleet CLI-surface line + the flag |

---

## Task 1: `ConcurrentMerger` trait + types

**Files:**
- Create: `mur-core/src/parallel/concurrent/mod.rs`
- Modify: `mur-core/src/parallel/mod.rs`

**Interfaces:**
- Produces:
  - `pub type ActorId = String;`
  - `pub struct OverlapRegion { pub base_line_range: std::ops::Range<u32>, pub actor_ids: Vec<ActorId> }`
  - `pub struct MergeOutcome { pub merged: Vec<u8>, pub overlaps: Vec<OverlapRegion> }`
  - `pub trait ConcurrentMerger { fn merge(&self, base: &[u8], versions: &[(ActorId, Vec<u8>)]) -> anyhow::Result<MergeOutcome>; }`

- [ ] **Step 1: Create the module with types + a smoke test**

Create `mur-core/src/parallel/concurrent/mod.rs`:

```rust
#![allow(dead_code, unused_imports)]
//! P3 concurrent merge (Model A): post-hoc N-way line merge of isolated agent
//! worktrees. Guarantees deterministic, order-independent convergence of the
//! merged bytes — NOT correctness. Disjoint hunks auto-merge; overlaps escalate.

pub mod hunk;
pub mod stats;
pub mod structural;

/// Stable per-track identity (the track name) — fixes tie-breaks for byte-stable output.
pub type ActorId = String;

/// A region of `base` where two or more actors made conflicting edits.
/// These are NEVER auto-merged; callers escalate them (judge/cherry/human).
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
```

In `mur-core/src/parallel/mod.rs`, add next to `pub mod cherry;`:

```rust
pub mod concurrent;
```

(`hunk`, `stats`, `structural` get real bodies in Tasks 2–4. To keep this task compiling, create each as a one-line stub now: `#![allow(dead_code)]` in `hunk.rs`, `stats.rs`, and `structural.rs`.)

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core --lib concurrent::tests::merge_outcome_is_clean`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/parallel/concurrent/ mur-core/src/parallel/mod.rs
git commit -m "feat(parallel/p3): ConcurrentMerger trait + MergeOutcome/OverlapRegion types

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 2: Line-hunk extraction + overlap grouping

**Files:**
- Modify: `mur-core/src/parallel/concurrent/hunk.rs` (replace stub)

**Interfaces:**
- Consumes: the `diff` crate (`diff::lines`, `diff::Result`).
- Produces:
  - `pub struct Hunk { pub base_start: u32, pub base_end: u32, pub replacement: Vec<String> }`
  - `pub struct Edit { pub actor: String, pub hunk: Hunk }`
  - `pub enum Group { Clean { hunk: Hunk, actors: Vec<String> }, Conflict { base_start: u32, base_end: u32, actors: Vec<String> } }`
  - `pub fn hunks_vs_base(base: &str, version: &str) -> Vec<Hunk>`
  - `pub fn group_edits(edits: Vec<Edit>) -> Vec<Group>`

Background on the `diff` crate: `diff::lines(base, version)` splits both with `str::lines()` (so line indices align with `base.lines().collect::<Vec<_>>()`) and returns `Vec<diff::Result<&str>>` where `Left(l)` = a base line removed, `Right(r)` = a line added, `Both(_,_)` = an unchanged base line.

- [ ] **Step 1: Write the failing tests**

Replace `mur-core/src/parallel/concurrent/hunk.rs` stub with:

```rust
#![allow(dead_code, unused_imports)]
//! Line-hunk extraction (vs a common base) and overlap classification.

/// A contiguous edit relative to `base`: replace base lines `[base_start, base_end)`
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
    /// All edits in the cluster are byte-identical → safe to apply once.
    Clean { hunk: Hunk, actors: Vec<String> },
    /// Edits disagree → an overlap that must be escalated, never auto-merged.
    Conflict { base_start: u32, base_end: u32, actors: Vec<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: u32, e: u32, repl: &[&str]) -> Hunk {
        Hunk { base_start: s, base_end: e, replacement: repl.iter().map(|x| x.to_string()).collect() }
    }

    #[test]
    fn replacement_hunk_maps_to_base_line() {
        let base = "a\nb\nc\n";
        let ver = "a\nB\nc\n"; // line 1 changed
        let hunks = hunks_vs_base(base, ver);
        assert_eq!(hunks, vec![h(1, 2, &["B"])]);
    }

    #[test]
    fn pure_insertion_is_zero_width() {
        let base = "a\nc\n";
        let ver = "a\nb\nc\n"; // insert "b" before line 1
        let hunks = hunks_vs_base(base, ver);
        assert_eq!(hunks, vec![h(1, 1, &["b"])]);
    }

    #[test]
    fn deletion_hunk_has_empty_replacement() {
        let base = "a\nb\nc\n";
        let ver = "a\nc\n"; // delete "b"
        let hunks = hunks_vs_base(base, ver);
        assert_eq!(hunks, vec![h(1, 2, &[])]);
    }

    #[test]
    fn disjoint_edits_form_two_clean_groups() {
        // actor x edits line 0, actor y edits line 2 — no overlap.
        let edits = vec![
            Edit { actor: "x".into(), hunk: h(0, 1, &["X"]) },
            Edit { actor: "y".into(), hunk: h(2, 3, &["Y"]) },
        ];
        let groups = group_edits(edits);
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| matches!(g, Group::Clean { .. })));
    }

    #[test]
    fn identical_edits_collapse_to_one_clean_group() {
        let edits = vec![
            Edit { actor: "x".into(), hunk: h(1, 2, &["SAME"]) },
            Edit { actor: "y".into(), hunk: h(1, 2, &["SAME"]) },
        ];
        let groups = group_edits(edits);
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            Group::Clean { hunk, actors } => {
                assert_eq!(hunk, &h(1, 2, &["SAME"]));
                assert_eq!(actors.len(), 2);
            }
            _ => panic!("expected clean"),
        }
    }

    #[test]
    fn divergent_overlapping_edits_form_conflict() {
        let edits = vec![
            Edit { actor: "x".into(), hunk: h(1, 2, &["FROM_X"]) },
            Edit { actor: "y".into(), hunk: h(1, 2, &["FROM_Y"]) },
        ];
        let groups = group_edits(edits);
        assert_eq!(groups.len(), 1);
        match &groups[0] {
            Group::Conflict { base_start, base_end, actors } => {
                assert_eq!((*base_start, *base_end), (1, 2));
                assert_eq!(actors.len(), 2);
            }
            _ => panic!("expected conflict"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core --lib concurrent::hunk`
Expected: FAIL — `hunks_vs_base` / `group_edits` not found.

- [ ] **Step 3: Implement extraction + grouping**

Insert above the `#[cfg(test)]` block:

```rust
/// Extract the edit hunks of `version` relative to `base`, indexed by base line.
/// Base line indices align with `base.lines().collect::<Vec<_>>()` because the
/// `diff` crate splits with `str::lines()`.
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
                h.base_end = base_idx + 1; // this base line is consumed
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

/// True if two hunks touch overlapping base ranges. Two pure insertions overlap
/// only at the same position; otherwise half-open ranges must intersect (a
/// boundary-touching insertion is treated as independent).
fn overlaps(a: &Hunk, b: &Hunk) -> bool {
    let a_ins = a.base_start == a.base_end;
    let b_ins = b.base_start == b.base_end;
    if a_ins && b_ins {
        a.base_start == b.base_start
    } else {
        a.base_start < b.base_end && b.base_start < a.base_end
    }
}

/// Cluster edits by overlapping base range; classify each cluster as Clean
/// (all edits byte-identical) or Conflict (any disagreement).
pub fn group_edits(mut edits: Vec<Edit>) -> Vec<Group> {
    edits.sort_by_key(|e| (e.hunk.base_start, e.hunk.base_end));
    let mut groups: Vec<Group> = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        // Grow a cluster while any subsequent edit overlaps any edit already in it.
        let mut cluster: Vec<Edit> = vec![edits[i].clone()];
        let mut max_end = edits[i].hunk.base_end.max(edits[i].hunk.base_start);
        let mut j = i + 1;
        while j < edits.len() && edits[j].hunk.base_start <= max_end {
            if cluster.iter().any(|c| overlaps(&c.hunk, &edits[j].hunk)) {
                max_end = max_end.max(edits[j].hunk.base_end);
                cluster.push(edits[j].clone());
                j += 1;
            } else {
                break;
            }
        }
        i = j;

        let first = &cluster[0].hunk;
        let all_identical = cluster.iter().all(|c| &c.hunk == first);
        let actors: Vec<String> = cluster.iter().map(|c| c.actor.clone()).collect();
        if all_identical {
            groups.push(Group::Clean { hunk: first.clone(), actors });
        } else {
            let base_start = cluster.iter().map(|c| c.hunk.base_start).min().unwrap();
            let base_end = cluster.iter().map(|c| c.hunk.base_end).max().unwrap();
            groups.push(Group::Conflict { base_start, base_end, actors });
        }
    }
    groups
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib concurrent::hunk`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/concurrent/hunk.rs
git commit -m "feat(parallel/p3): line-hunk extraction + overlap classification (zero-dep)

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 3: `StructuralMerger` (zero-dep `ConcurrentMerger`)

**Files:**
- Modify: `mur-core/src/parallel/concurrent/structural.rs` (replace stub)

**Interfaces:**
- Consumes: `hunk::{hunks_vs_base, group_edits, Edit, Group}`, `super::{ConcurrentMerger, MergeOutcome, OverlapRegion, ActorId}`.
- Produces: `pub struct StructuralMerger;` implementing `ConcurrentMerger`.

- [ ] **Step 1: Write the failing tests**

Replace `mur-core/src/parallel/concurrent/structural.rs` stub with:

```rust
#![allow(dead_code, unused_imports)]
//! Zero-dependency ConcurrentMerger: splice disjoint clean hunks into the base,
//! report overlaps for escalation. Deterministic, order-independent convergence
//! of merged bytes — NOT correctness.

use super::hunk::{Edit, Group, group_edits, hunks_vs_base};
use super::{ActorId, ConcurrentMerger, MergeOutcome, OverlapRegion};
use anyhow::Result;

pub struct StructuralMerger;

#[cfg(test)]
mod tests {
    use super::*;

    fn vers(pairs: &[(&str, &str)]) -> Vec<(ActorId, Vec<u8>)> {
        pairs.iter().map(|(a, s)| (a.to_string(), s.as_bytes().to_vec())).collect()
    }

    #[test]
    fn disjoint_edits_all_merge() {
        let base = b"a\nb\nc\n";
        let v = vers(&[("x", "A\nb\nc\n"), ("y", "a\nb\nC\n")]);
        let out = StructuralMerger.merge(base, &v).unwrap();
        assert!(out.is_clean());
        assert_eq!(String::from_utf8(out.merged).unwrap(), "A\nb\nC\n");
    }

    #[test]
    fn overlapping_edits_escalate_and_base_kept() {
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
        let out1 = StructuralMerger.merge(base, &vers(&[("x", "A\nb\nc\n"), ("y", "a\nb\nC\n")])).unwrap();
        let out2 = StructuralMerger.merge(base, &vers(&[("y", "a\nb\nC\n"), ("x", "A\nb\nc\n")])).unwrap();
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core --lib concurrent::structural`
Expected: FAIL — `ConcurrentMerger` not implemented for `StructuralMerger`.

- [ ] **Step 3: Implement the merger**

Insert above the `#[cfg(test)]` block:

```rust
impl ConcurrentMerger for StructuralMerger {
    fn merge(&self, base: &[u8], versions: &[(ActorId, Vec<u8>)]) -> Result<MergeOutcome> {
        let base_str = std::str::from_utf8(base).map_err(|e| anyhow::anyhow!("base not UTF-8: {e}"))?;

        // Collect every actor's hunks vs base.
        let mut edits: Vec<Edit> = Vec::new();
        for (actor, bytes) in versions {
            let v = std::str::from_utf8(bytes)
                .map_err(|e| anyhow::anyhow!("version {actor} not UTF-8: {e}"))?;
            for h in hunks_vs_base(base_str, v) {
                edits.push(Edit { actor: actor.clone(), hunk: h });
            }
        }

        let groups = group_edits(edits);

        // Partition into clean hunks (apply) and overlaps (escalate).
        let mut clean: Vec<super::hunk::Hunk> = Vec::new();
        let mut overlaps: Vec<OverlapRegion> = Vec::new();
        for g in groups {
            match g {
                Group::Clean { hunk, .. } => clean.push(hunk),
                Group::Conflict { base_start, base_end, actors } => {
                    overlaps.push(OverlapRegion { base_line_range: base_start..base_end, actor_ids: actors });
                }
            }
        }

        // Splice clean hunks into the base, highest base offset first so earlier
        // indices stay valid. Wider ranges before zero-width insertions at the
        // same start for stable output.
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib concurrent::structural`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/concurrent/structural.rs
git commit -m "feat(parallel/p3): zero-dep StructuralMerger — disjoint auto-merge, overlap escalate

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 4: `OverlapStats` (Spike-1 instrumentation)

**Files:**
- Modify: `mur-core/src/parallel/concurrent/stats.rs` (replace stub)

**Interfaces:**
- Consumes: `super::{ConcurrentMerger, MergeOutcome}`, `structural::StructuralMerger`.
- Produces:
  - `pub struct OverlapStats { pub n_tracks: usize, pub files_compared: usize, pub clean_groups: usize, pub overlap_regions: usize, pub overlap_rate: f64 }` (Serialize)
  - `pub fn accumulate(stats: &mut OverlapStats, outcome: &MergeOutcome, clean_groups_in_file: usize)`
  - `pub fn count_clean_groups(merger: &StructuralMerger, base: &[u8], versions: &[(String, Vec<u8>)]) -> anyhow::Result<(usize, usize)>` → `(clean_groups, overlaps)`

- [ ] **Step 1: Write the failing test**

Replace `mur-core/src/parallel/concurrent/stats.rs` stub with:

```rust
#![allow(dead_code, unused_imports)]
//! Spike-1 instrumentation: how often do agent edits actually overlap?
//! If overlap is rare or N≈2, the CRDT (Loro) engine is not worth adding.

use super::structural::StructuralMerger;
use super::{ConcurrentMerger, MergeOutcome};
use anyhow::Result;
use serde::Serialize;

/// Aggregate overlap statistics across the files of one parallel run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct OverlapStats {
    pub n_tracks: usize,
    pub files_compared: usize,
    pub clean_groups: usize,
    pub overlap_regions: usize,
    /// overlap_regions / (clean_groups + overlap_regions); 0.0 when no edits.
    pub overlap_rate: f64,
}

impl OverlapStats {
    pub fn finalize(&mut self) {
        let total = self.clean_groups + self.overlap_regions;
        self.overlap_rate = if total > 0 {
            self.overlap_regions as f64 / total as f64
        } else {
            0.0
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vers(pairs: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
        pairs.iter().map(|(a, s)| (a.to_string(), s.as_bytes().to_vec())).collect()
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
        let mut s = OverlapStats { clean_groups: 3, overlap_regions: 1, ..Default::default() };
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core --lib concurrent::stats`
Expected: FAIL — `count_groups` not found.

- [ ] **Step 3: Implement the counter**

Insert above the `#[cfg(test)]` block:

```rust
/// Merge one file and report (clean_groups_applied, overlap_regions).
/// clean_groups = lines that changed cleanly = merged outcome's applied hunks,
/// derived as (distinct edit clusters) - (overlaps).
pub fn count_groups(
    merger: &StructuralMerger,
    base: &[u8],
    versions: &[(String, Vec<u8>)],
) -> Result<(usize, usize)> {
    let outcome: MergeOutcome = merger.merge(base, versions)?;
    let overlaps = outcome.overlaps.len();
    // Recover clean-group count: re-run grouping to count clean clusters.
    let base_str = std::str::from_utf8(base)?;
    let mut edits = Vec::new();
    for (actor, bytes) in versions {
        let v = std::str::from_utf8(bytes)?;
        for h in super::hunk::hunks_vs_base(base_str, v) {
            edits.push(super::hunk::Edit { actor: actor.clone(), hunk: h });
        }
    }
    let clean = super::hunk::group_edits(edits)
        .iter()
        .filter(|g| matches!(g, super::hunk::Group::Clean { .. }))
        .count();
    Ok((clean, overlaps))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core --lib concurrent::stats`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/concurrent/stats.rs
git commit -m "feat(parallel/p3): OverlapStats — Spike-1 overlap-rate instrumentation

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 5: `mur fleet merge-concurrent` command

**Files:**
- Create: `mur-core/src/cmd/fleet/concurrent_cmd.rs`
- Modify: `mur-core/src/cmd/fleet/cherry_cmd.rs` (`pub(super)` on two helpers — skip if Task already done by the P2.5 plan)
- Modify: `mur-core/src/cmd/fleet/mod.rs`
- Modify: `mur-core/src/cli/actions.rs`
- Modify: `mur-core/src/dispatch.rs`

**Interfaces:**
- Consumes: `TrackSet::load`, `StructuralMerger`, `OverlapStats`, `cherry_cmd::{promote_cherry_result, project_root_from_worktree}`.
- Produces: `pub fn cmd_fleet_merge_concurrent(mur_home: &Path, fleet_name: &str, stats: bool, promote: bool, target: Option<&Path>) -> anyhow::Result<()>`

The command:
1. Refuse unless `std::env::var("MUR_PARALLEL_CONCURRENT").as_deref() == Ok("1")`.
2. Load `tracks.json`. Read the base commit sha from the first track's `.parallel-base` sentinel.
3. Compute the union of changed files across tracks (`git diff --name-only <base> HEAD` per track worktree).
4. For each changed file: base content = `git show <base>:<relpath>` (run in track[0]); per-track version = the file in each track worktree (fall back to base content if a track didn't change it). Merge with `StructuralMerger`.
5. Write merged files to `cherry-result/`; collect `OverlapStats`.
6. `--stats` → write `concurrent_stats.json`.
7. `--promote` → refuse if any overlaps (Gate 3) or dest dirty; copy; run `cargo check` (Gate 1) in dest; on failure `git checkout --` the copied files and report.

- [ ] **Step 1: Make the cherry promote helpers reusable (idempotent)**

In `mur-core/src/cmd/fleet/cherry_cmd.rs`, ensure both helpers are `pub(super)` (no-op if the P2.5 plan already did this):

```rust
pub(super) fn promote_cherry_result(result_dir: &Path, dest: &Path) -> Result<()> {
```
```rust
pub(super) fn project_root_from_worktree(worktree: &Path) -> Option<PathBuf> {
```

- [ ] **Step 2: Write the failing test (pure helpers)**

Create `mur-core/src/cmd/fleet/concurrent_cmd.rs`:

```rust
//! `mur fleet merge-concurrent <name>` — Model A post-hoc N-way line merge.
//! Default off; requires MUR_PARALLEL_CONCURRENT=1. Disjoint hunks auto-merge;
//! any overlap refuses --promote and is reported for escalation.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::store::load_fleet;
use crate::parallel::concurrent::stats::{OverlapStats, count_groups};
use crate::parallel::concurrent::structural::StructuralMerger;
use crate::parallel::concurrent::ConcurrentMerger;
use crate::parallel::track::TrackSet;

const FLAG_ENV: &str = "MUR_PARALLEL_CONCURRENT";

fn flag_enabled() -> bool {
    std::env::var(FLAG_ENV).as_deref() == Ok("1")
}

/// `git show <rev>:<relpath>` run in `cwd`; None if the path didn't exist at rev.
fn git_show(cwd: &Path, rev: &str, relpath: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("git")
        .arg("show")
        .arg(format!("{rev}:{relpath}"))
        .current_dir(cwd)
        .output()
        .ok()?;
    if out.status.success() { Some(out.stdout) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_gates_the_command() {
        // SAFETY: single-threaded test; restore after.
        unsafe { std::env::remove_var(FLAG_ENV) };
        assert!(!flag_enabled());
        unsafe { std::env::set_var(FLAG_ENV, "1") };
        assert!(flag_enabled());
        unsafe { std::env::remove_var(FLAG_ENV) };
    }
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p mur-core --lib concurrent_cmd`
Expected: FAIL first (module not declared) → declare it, then PASS.

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod concurrent_cmd;
```

Re-run: PASS.

- [ ] **Step 4: Implement the command**

Add to `concurrent_cmd.rs` (above the test module):

```rust
const PARALLEL_BASE_FILE: &str = ".parallel-base";

pub fn cmd_fleet_merge_concurrent(
    mur_home: &Path,
    fleet_name: &str,
    stats: bool,
    promote: bool,
    target: Option<&Path>,
) -> Result<()> {
    if !flag_enabled() {
        anyhow::bail!(
            "concurrent merge is experimental and off by default — set {FLAG_ENV}=1 to enable"
        );
    }
    let _fleet = load_fleet(mur_home, fleet_name)?;
    let fleet_dir = mur_home.join("fleets").join(fleet_name);
    let tracks = TrackSet::load(&fleet_dir).context("no tracks.json — run `mur fleet run` first")?;
    if tracks.tracks.len() < 2 {
        anyhow::bail!("concurrent merge needs ≥2 tracks");
    }
    let t0 = &tracks.tracks[0];
    let base_rev = std::fs::read_to_string(t0.worktree_path.join(PARALLEL_BASE_FILE))
        .context("read .parallel-base sentinel — was the run created by `mur fleet run`?")?
        .trim()
        .to_string();

    // Union of changed files across tracks (relative paths).
    let mut changed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for t in &tracks.tracks {
        let out = std::process::Command::new("git")
            .args(["diff", "--name-only", &base_rev, "HEAD"])
            .current_dir(&t.worktree_path)
            .output()
            .context("git diff --name-only")?;
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.ends_with(".rs") {
                changed.insert(line.to_string());
            }
        }
    }
    if changed.is_empty() {
        println!("No changed .rs files to merge.");
        return Ok(());
    }

    let merger = StructuralMerger;
    let result_dir = fleet_dir.join("cherry-result");
    let mut stat = OverlapStats { n_tracks: tracks.tracks.len(), ..Default::default() };
    let mut any_overlap = false;
    let mut written: Vec<String> = Vec::new();

    for rel in &changed {
        let base = git_show(&t0.worktree_path, &base_rev, rel).unwrap_or_default();
        let versions: Vec<(String, Vec<u8>)> = tracks
            .tracks
            .iter()
            .map(|t| {
                let p = t.worktree_path.join(rel);
                let bytes = std::fs::read(&p).unwrap_or_else(|_| base.clone());
                (t.config.name.clone(), bytes)
            })
            .collect();

        let outcome = merger.merge(&base, &versions)?;
        let (clean, overlaps) = count_groups(&merger, &base, &versions)?;
        stat.files_compared += 1;
        stat.clean_groups += clean;
        stat.overlap_regions += overlaps;

        if !outcome.overlaps.is_empty() {
            any_overlap = true;
            println!("⚠ {rel}: {} overlap region(s) — escalate via `mur fleet judge`/`compare`", outcome.overlaps.len());
            for o in &outcome.overlaps {
                println!("    base lines {}–{} touched by {}", o.base_line_range.start + 1, o.base_line_range.end, o.actor_ids.join(", "));
            }
        }

        let out_path = result_dir.join(rel);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, &outcome.merged)?;
        written.push(rel.clone());
    }
    stat.finalize();

    println!(
        "Merged {} file(s) → {} ({} clean groups, {} overlaps, overlap_rate {:.2})",
        stat.files_compared, result_dir.display(), stat.clean_groups, stat.overlap_regions, stat.overlap_rate
    );

    if stats {
        let path = fleet_dir.join("concurrent_stats.json");
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&stat)?)?;
        std::fs::rename(&tmp, &path)?;
        eprintln!("stats written to {}", path.display());
    }

    if promote {
        if any_overlap {
            anyhow::bail!("refusing to promote: unresolved overlaps — resolve via judge/cherry first");
        }
        let dest = match target {
            Some(p) => p.to_path_buf(),
            None => super::cherry_cmd::project_root_from_worktree(&t0.worktree_path)
                .context("cannot determine project root — pass --target <path>")?,
        };
        super::cherry_cmd::promote_cherry_result(&result_dir, &dest)?;
        // Gate 1: cargo check in dest; revert exactly our files on failure.
        let status = std::process::Command::new("cargo")
            .arg("check")
            .current_dir(&dest)
            .status();
        match status {
            Ok(s) if s.success() => println!("cargo check: OK"),
            Ok(_) => {
                for rel in &written {
                    let _ = std::process::Command::new("git")
                        .args(["checkout", "--", rel])
                        .current_dir(&dest)
                        .status();
                }
                anyhow::bail!("cargo check failed — reverted promoted files");
            }
            Err(e) => eprintln!("cargo check could not run ({e}) — left files in place"),
        }
    } else {
        println!("Run with --promote to copy into the project (refused if overlaps remain).");
    }
    Ok(())
}
```

In `mur-core/src/cli/actions.rs`, add to `FleetAction`:

```rust
    /// Model A concurrent merge of a parallel run (experimental; MUR_PARALLEL_CONCURRENT=1)
    MergeConcurrent {
        /// Fleet name
        name: String,
        /// Write concurrent_stats.json (Spike-1 overlap rate)
        #[arg(long)]
        stats: bool,
        /// Copy the merged result into the live project (refused if overlaps remain)
        #[arg(long)]
        promote: bool,
        /// Override destination for --promote
        #[arg(long)]
        target: Option<std::path::PathBuf>,
    },
```

In `mur-core/src/dispatch.rs`, add the arm:

```rust
                FleetAction::MergeConcurrent { name, stats, promote, target } => {
                    cmd::fleet::concurrent_cmd::cmd_fleet_merge_concurrent(
                        &mur_home,
                        &name,
                        stats,
                        promote,
                        target.as_deref(),
                    )?
                }
```

- [ ] **Step 5: Run tests + build**

Run: `cargo test -p mur-core --lib concurrent && cargo build -p mur-core`
Expected: PASS + clean build.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/fleet/concurrent_cmd.rs mur-core/src/cmd/fleet/cherry_cmd.rs mur-core/src/cmd/fleet/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(parallel/p3): mur fleet merge-concurrent (flag-gated) + Spike-1 --stats

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Task 6: Spike-1 decision doc + gate script + CLAUDE.md

**Files:**
- Create: `docs/superpowers/validation/spike1-overlap-rate.md`
- Create: `scripts/gate7_concurrent_merge.sh`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Write the gate script**

Create `scripts/gate7_concurrent_merge.sh`:

```bash
#!/usr/bin/env bash
# Gate 7: P3 concurrent merge — zero-dep round-trip (no live agents).
# Pass criterion: all concurrent-merge unit tests green.
set -euo pipefail
echo "Gate 7: concurrent merge (StructuralMerger + hunk + stats)"
cargo test -p mur-core --lib concurrent:: -- --nocapture
cargo test -p mur-core --lib concurrent_cmd -- --nocapture
echo ""
echo "GATE 7: ✅ PASS"
```

- [ ] **Step 2: Run the gate**

Run: `bash scripts/gate7_concurrent_merge.sh`
Expected: all concurrent tests pass, prints `GATE 7: ✅ PASS`.

- [ ] **Step 3: Write the Spike-1 decision doc**

Create `docs/superpowers/validation/spike1-overlap-rate.md`:

```markdown
# Spike-1 — Concurrent-edit overlap rate (decides whether CRDT is worth building)

**Question:** In real mur parallel runs, how often do ≥2 agents edit overlapping
lines, and what is the distribution of N (tracks per run)? If overlap is rare or
N≈2, the zero-dep StructuralMerger already captures the auto-accept value and the
Loro engine (Phase 1) is NOT worth its ~40+ transitive dependencies.

## How to run

1. Execute real parallel runs (`mur fleet run <fleet>` with ≥2 member agents and a
   `parallel:` config) so `~/.mur/fleets/<name>/tracks.json` + worktrees exist.
2. `MUR_PARALLEL_CONCURRENT=1 mur fleet merge-concurrent <name> --stats`
3. Read `~/.mur/fleets/<name>/concurrent_stats.json`:
   `{ n_tracks, files_compared, clean_groups, overlap_regions, overlap_rate }`.
4. Aggregate across ≥10 runs (varied tasks/fleets).

## Decision gate

| Observed | Decision |
|---|---|
| overlap_rate consistently < ~0.10, or N almost always 2 | **STOP** — ship the zero-dep StructuralMerger as the concurrent reconciler; do NOT build the Loro engine. |
| overlap_rate frequently ≥ ~0.10 at N>2 | **PROCEED to Phase 1** — write the Loro spike plan (Spike-2 footprint, Spike-3 diff→ops fidelity). |

## Results (fill in)

| run | fleet | N | files | clean_groups | overlap_regions | overlap_rate |
|-----|-------|---|-------|--------------|-----------------|--------------|
|     |       |   |       |              |                 |              |

**Conclusion:** _(STOP / PROCEED + one-line rationale)_
```

- [ ] **Step 4: Update CLAUDE.md**

In `CLAUDE.md`, append to the `mur fleet { … }` bullet:

```
**Concurrent merge (P3 Phase 0, experimental, default OFF):** `mur fleet merge-concurrent <name> [--stats] [--promote] [--target <path>]` (requires `MUR_PARALLEL_CONCURRENT=1`) runs a zero-dependency Model-A post-hoc N-way LINE merge of the parallel run's worktrees: disjoint hunks auto-merge, ANY overlapping region is reported and escalated (never silently merged), `--promote` refuses on unresolved overlaps and reverts on `cargo check` failure. `--stats` writes `concurrent_stats.json` (Spike-1 overlap rate — gates whether the Loro CRDT engine in Phase 1 is ever built). Guarantees deterministic order-independent convergence of merged bytes, NOT correctness. See `docs/superpowers/specs/2026-06-29-parallel-tracks-p3-concurrent-merge-design.md`.
```

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/validation/spike1-overlap-rate.md scripts/gate7_concurrent_merge.sh CLAUDE.md
git commit -m "docs(parallel/p3): Spike-1 decision doc + gate7 script + CLAUDE.md surface

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>"
```

---

## Not in this plan (conditional Phase 1)

The **Loro CRDT engine** (`LoroMerger`, the `concurrent-loro` cargo feature, Spike-2 footprint, Spike-3 diff→ops fidelity) is intentionally excluded. It is written **only if Spike-1 shows overlap is common at N>2**. Under the safe gate policy (overlaps always escalate), the zero-dep `StructuralMerger` already delivers the entire auto-accept value, so the CRDT is a pure enhancement for *auto-merging overlaps* — a capability we should not build until data justifies the dependency. When Spike-1 says PROCEED, write a separate plan starting from spec §9 Spike-2/3.

---

## Self-Review

**Spec coverage** (against `2026-06-29-parallel-tracks-p3-concurrent-merge-design.md`):
- §4 Model A (post-hoc, isolated) → Tasks 3, 5 operate on final worktree states. ✓
- §5.1 `ConcurrentMerger` trait + `MergeOutcome`/`OverlapRegion` → Task 1 (matches spec field names). ✓
- §5.2 line granularity → Task 2 `hunks_vs_base` is line-based. ✓
- §5.3 deterministic ActorId → `ActorId = track name`; Task 3 `order_independent_output` test. ✓
- §6.2 disjoint auto-merge, overlap escalate → Task 3 `overlapping_edits_escalate`. ✓
- §7 gate: Gate 1 cargo check (Task 5 promote), Gate 3 overlap escalation (Task 5 refuse-on-overlap). Gate 2 (tests) is operator-run, noted. ✓
- §8 surface: `mur fleet merge-concurrent`, `MUR_PARALLEL_CONCURRENT=1`, no new ParallelMode, promote reuse → Task 5. ✓
- §9 Spike-1 instrumentation + decision gate → Tasks 4, 6. ✓
- Copy discipline ("convergence not correctness") → enforced in module docs, CLAUDE.md, output strings. ✓
- Zero new deps (uses existing `diff`/`git2`) → Global Constraints. ✓
- §9 Spike-2/3 + Loro → explicitly deferred to conditional Phase 1. ✓

**Placeholder scan:** No TODO/TBD; every code step is complete. The Spike-1 results table is an intentional fill-in *artifact* (operator data), not a plan placeholder.

**Type consistency:**
- `Hunk { base_start: u32, base_end: u32, replacement: Vec<String> }` — identical across Tasks 2–4. ✓
- `Group::{Clean{hunk,actors}, Conflict{base_start,base_end,actors}}` — Tasks 2, 4. ✓
- `MergeOutcome { merged: Vec<u8>, overlaps: Vec<OverlapRegion> }` / `OverlapRegion { base_line_range: Range<u32>, actor_ids }` — Tasks 1, 3, 5 (matches spec §5.1). ✓
- `ConcurrentMerger::merge(&self, base: &[u8], versions: &[(ActorId, Vec<u8>)])` — Tasks 1, 3, 4, 5. ✓
- `count_groups(&StructuralMerger, base, versions) -> (clean, overlaps)` — Tasks 4, 5. ✓
- Reused `cherry_cmd::{promote_cherry_result, project_root_from_worktree}` signatures match the P2.5 plan / PR #558. ✓

**Verification points for the implementer (flagged):**
- The `diff` crate (`diff = "0.1"`) exposes `diff::lines(&str,&str) -> Vec<diff::Result<&str>>` with `Result::{Left,Both,Right}` — confirm the import path is `diff::Result` (Task 2). If the local version differs, adjust the match arms.
- `std::env::set_var`/`remove_var` are `unsafe` in edition 2024 — Task 5 test wraps them in `unsafe {}`. Keep that test single-threaded (it is, by default per-test).
