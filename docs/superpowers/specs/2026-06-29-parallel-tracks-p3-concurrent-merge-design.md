# Parallel Tracks P3 — Concurrent Merge (CRDT, Spike-Gated) Design

**Date:** 2026-06-29
**Status:** Approved design — ready for implementation plan
**Supersedes/extends:** P1 (speculative + judge/cherry), P2.5 (semantic partition). Production-hardening is **out of scope** and gets its own spec.

---

## 1. Summary

P3 adds a **third reconciler** to parallel tracks — *concurrent merge* — that merges **overlapping** edits from multiple agents that partition mode (disjoint regions) and speculative cherry-pick (whole-unit selection) cannot. It follows **Model A (post-hoc CRDT N-way merge)**: agents stay isolated in their worktrees and each produces a final file state; after they finish, we diff each version against the common base and merge all N **commutatively and order-independently**, then gate the result with `cargo check`.

The entire CRDT investment is **gated behind a measurement spike**. Industry prior art and our own analysis agree that the unique CRDT advantage (N-way overlapping merge) may rarely trigger in practice. So the design's first deliverables are three spikes; the production-quality Loro integration is built **only if Spike-1 proves overlap is real**.

This mode is **opt-in, default-off** (`MUR_PARALLEL_CONCURRENT=1`), sits alongside the existing reconcilers, and never replaces them.

### Honest framing (carried verbatim into all docs)

- The merge guarantees **deterministic, order-independent convergence of the merged bytes** — **NOT** correctness. Never write "correct merge" in code, docs, or UI.
- "final-state diff → CRDT → commutative merge" is, to our research, **a novel approach in the AI-agent setting** (no known agent system does it). It ships behind a flag, beside the proven reconcilers, never as a default.

---

## 2. Goals / Non-Goals

**Goals**
- Merge N>2 concurrent agent versions of the same file deterministically and order-independently.
- Auto-merge **disjoint** edit hunks; **escalate overlapping** regions to the existing judge+cherry path rather than silently interleaving.
- Keep the engine swappable behind a `ConcurrentMerger` trait so the spike can choose Loro vs `cola` vs a self-written zero-dep merger without reworking callers.
- Preserve the existing isolated-worktree, final-file-state architecture unchanged — agents need zero modification.

**Non-Goals**
- **Production mode** (gating maturity, persistence, observability, failure recovery, rate limiting) — separate spec; it is cross-cutting across all three reconcilers.
- **Live shared-document co-editing** (Model B) and **Edit-tool op interception** (Model C) — rejected (§4).
- **AST/structural merge engine** — over-engineering for v1 (§6.4). Structural checks may appear later only as an optional overlap-region *validator*, never the default engine.
- Replacing judge/cherry (speculative) or partition. P3 is additive.

---

## 3. Background: where this slots in

The parallel-tracks pipeline runs N agents in isolated git worktrees (`backend::create_track`), each editing files with Edit/Write tools, producing a **final file state**. Reconciliation today:

- **Speculative (P1):** judge scores each semantic unit across tracks → `cherry_pick` selects the best track per unit → `assemble_file` splices. Whole-unit granularity.
- **Partition (P2.5):** disjoint regions assigned up front → each agent owns a region → splice by assignment. No overlap permitted.

Neither can merge two agents who edited *the same function* in compatible-but-different ways at sub-unit granularity. That is the gap P3 fills.

Reused infrastructure (no reimplementation):
- `backend::ParallelBackend::{base_snapshot, diff_files}` — common base + changed files per track.
- `parallel::semantic::extract_units` — region boundaries when escalating overlaps to unit granularity.
- `parallel::cherry::{cherry_pick, assemble::assemble_file}` — the escalation target for overlapping regions.
- The `cargo check` gate already implemented in `cmd/fleet/cherry_cmd.rs`.

---

## 4. Concurrency model decision — Model A

| Model | What it is | Fit with mur | Verdict |
|---|---|---|---|
| **A — post-hoc CRDT N-way merge** | Agents isolated; after completion each file is diffed vs base, replayed as a concurrent actor into a CRDT, merged commutatively, then `cargo check`-gated | **Additive**: reuses "isolated worktree → final file state" verbatim; CRDT is a third reconciler beside judge/cherry and partition | **Chosen** |
| **B — live shared CRDT document** | Agents co-edit one live document in real time (Yjs/Hocuspocus-style relay + claim coordination) | Requires rewriting agents from "produce final file" to "stream ops into a shared doc" + a live coordination protocol | Rejected: invasive; payoff unproven; LLM agents are confused by a doc changing under them |
| **C — intercept Edit/Write as op stream** | Translate each tool call into a CRDT op in real time | Loses the "final state" contract; couples merge to tool-call timing; brittle. Model A already reconstructs ops from final state via a line diff | Rejected |

Industry consensus (Claude Code sequential file-list merge, Cursor best-of-N *select* not *merge*, Cognition/Devin "don't let multiple agents write simultaneously", and the strongest academic result CAID using `git worktree`+`merge` then integrate) converges on **isolate + avoid overlap + test-gated integration**. Model A preserves isolation and only *adds* optional auto-merge for the overlaps partition can't catch. B and C fight that consensus.

---

## 5. Architecture (Model A)

```
agent-0 worktree ┐
agent-1 worktree ─┼─► per changed file:  base + [v0..vN]
agent-2 worktree ┘            │
                              ▼
                  hunk classification (disjoint vs overlapping)
                   │                                  │
             disjoint hunks                    overlapping regions
                   │                                  │
        ConcurrentMerger (engine)            escalate → extract_units →
        N-way commutative merge               judge+cherry (or human)
                   │                                  │
                   └───────────────┬──────────────────┘
                                   ▼
                         merged file  →  Gate 1: cargo check + clippy
                                          Gate 2: tests
                                          Gate 3: overlap escalation
                                   ▼
                    auto-accept ONLY if disjoint AND Gate 1+2 green
```

### 5.1 `ConcurrentMerger` trait (engine boundary)

The merge engine is abstracted so the spike picks the implementation without touching callers:

```rust
/// Merge N independently-edited versions of one file against a common base.
/// Returns bytes that converge deterministically and order-independently —
/// NOT a guarantee of correctness.
pub trait ConcurrentMerger {
    /// `base`: common ancestor. `versions`: (stable_actor_id, final_bytes) per track.
    /// `actor_id` MUST be a fixed per-track identity for byte-stable output.
    fn merge(
        &self,
        base: &[u8],
        versions: &[(ActorId, Vec<u8>)],
    ) -> anyhow::Result<MergeOutcome>;
}

pub struct MergeOutcome {
    pub merged: Vec<u8>,
    /// Regions where ≥2 actors touched overlapping lines — these are NOT
    /// auto-merged; callers escalate them to judge/cherry.
    pub overlaps: Vec<OverlapRegion>,
}

pub struct OverlapRegion {
    /// Line range in `base` whose edits collided.
    pub base_line_range: std::ops::Range<u32>,
    pub actor_ids: Vec<ActorId>,
}
```

Candidate implementations (chosen by spike): `LoroMerger` (feature `concurrent-loro`), or `StructuralMerger` (zero-dep, self-written) if Spike-1/2 favor it.

### 5.2 Merge granularity — **line**, not character

Character-level maximizes interleaving anomalies (Kleppmann's `"Hello Al Ciharcliee!"`); Weidner proves **no doubly-non-interleaving list CRDT exists**, so no algorithm fully avoids interleaving. Line granularity sharply reduces it, shrinks metadata, and yields cleaner `cargo check`. Loro exposes `LoroText::update_by_line` for exactly this. *(Medium-high confidence; no paper benchmarks line-granularity code merge specifically — see Spike-3.)*

### 5.3 Determinism

The final merged string can depend on actor/PeerID tie-breaks. Each track is assigned a **fixed, deterministic ActorId** (e.g. derived from track name/index) so output is byte-stable across runs and restarts. Verified in Spike-3.

### 5.4 Loro mapping (if greenlit)

Model A's five steps map one-to-one to named Loro APIs — no hand-written CRDT:

1. `let base = LoroDoc::new(); base.get_text("f").insert(0, base_src)?;`
2. `let fork_i = base.fork();` per track — `fork()` gives each a distinct PeerID → an independent concurrent actor.
3. `fork_i.get_text("f").update_by_line(version_i, opts)?` — internal current→final line/Myers diff emits concurrent ops. *This is the "agent gives a final file, not an op stream" primitive.* Use `update_by_line` (plain `update` can be slow / `UpdateTimeoutError` on >50k chars).
4. `merged.import_batch(&[...])` in **arbitrary order** — docs: "The import result will be the same."
5. `merged.get_text("f").to_string()` → Gate 1.

---

## 6. Source-merge best practices (baked into the design)

1. **Granularity = line** (§5.2).
2. **Disjoint hunks auto-merge; true overlap escalates.** Overwhelmingly, N diffs touch non-adjacent line ranges → splice and ship. Any region where ≥2 agents touched overlapping lines → record as a jj-style multi-sided conflict and route to **judge+cherry**. (ASE-2024: a wrong auto-merge costs ~2–6× a flagged conflict; CodeCRDT: complex tasks ~80% semantic conflicts. Auto-accepting all overlap is net-negative.)
3. **Fixed deterministic ActorId** for byte-stable output (§5.3).
4. **diff→ops fidelity is the key unknown** (Spike-3): Fugue's non-interleaving guarantee is defined over *live* ops carrying true left/right origins; ops reconstructed from a base-vs-final diff may not reproduce those origin anchors, weakening interleaving resistance. All three adversarial review lenses independently flagged this.
5. **Build-vs-buy stays open** until Spike-2: we consume a single property ("commutative N-way line merge"); a self-written Fugue-lite or a jj "conflict-as-data" approach may best honor the zero-dependency discipline.
6. **AST/structural merge is over-engineering here.** No maintained, pure-Rust, permissive AST-*merge* crate exists (Mergiraf is solid but **GPLv3** → subprocess-only, and tree-sitter grammars often pull generated C parsers, conflicting with "no C toolchain"). "Always structural" *increases* silent mis-merges (ASE-2024). Structural checks, if ever added, are an **optional overlap-region validator**, not the default engine.

---

## 7. Gating — convergence ≠ correctness

Correctness lives entirely in layered, blocking, escalating **post-merge gates**:

- **Gate 1 — `cargo check` + `clippy` (hard, blocking).** Rust is a genuine advantage: `rustc` statically rejects the three failure classes CodeCRDT names (duplicate declarations, type mismatch, broken references) — strictly stronger than a TS-diagnostics gate. A non-compiling merge is rejected and never surfaces. **Load-bearing, not advisory.**
- **Gate 2 — tests (hard, with an independence caveat).** Run tests, but note they are often written by the *same* coder agents (shared blind spots) — **not an independent oracle**. Prefer independently-curated acceptance/contract tests where available.
- **Gate 3 — semantic-divergence escalation.** Gates 1–2 miss "two compilable but divergent refactors" and "right code in the wrong place" (Coghlan's `memcpy`-in-wrong-function: compiles and is wrong). **Any region where ≥2 agents' diffs overlap is never auto-accepted** — escalate to judge/cherry/human.

**Net policy:** auto-accept **only** when (a) hunks are disjoint **and** (b) Gate 1+2 are green. Any overlap or any red → fall back to judge/human. **LLM conflict-fixing is not a cheap backstop** (DeepMerge-class: ~36–68% on simple conflicts, ~15% on non-trivial).

---

## 8. Integration surface

- **Flag:** `MUR_PARALLEL_CONCURRENT=1` (default off). Absent → command refuses with a one-line explanation.
- **Cargo feature:** `concurrent-loro` gates the Loro dependency so the workspace builds with zero new deps until the feature is explicitly enabled.
- **Command:** `mur fleet merge-concurrent <name> [--promote] [--target <path>]` — mirrors P2.5's `fleet merge`. Loads `tracks.json`, computes base via `backend::base_snapshot`, reads each track's changed files via `diff_files`, runs the `ConcurrentMerger`, writes to `cherry-result/`, and reuses `cherry_cmd::{promote_cherry_result, project_root_from_worktree}` for `--promote`.
- **Escalation reuse:** overlapping regions feed `extract_units` + `cherry_pick` + `assemble_file` — the existing speculative path.
- **No new `ParallelMode`.** Concurrent merge is a *reconciler chosen at merge time*, not an execution mode — it applies to the output of any parallel run.

---

## 9. The three spikes (first deliverables — gate everything after)

Each spike is a small, throwaway-or-keep measurement with an explicit decision gate.

### Spike-1 — overlap rate (HIGHEST PRIORITY; gates whether CRDT is built at all)
Instrument real parallel runs: record the distribution of N (agents per run) and the **same-line overlap rate** across tracks' diffs vs base.
- **Decision gate:** if overlap is rare or N is typically 2, the unique CRDT advantage almost never triggers → take the **zero-dep structural merge** path (or shelve P3). If overlap is common at N>2 → proceed to Spike-2/3.

### Spike-2 — Loro footprint vs zero-dep discipline
Behind the `concurrent-loro` feature, measure `cargo tree`, `cargo bloat`, and `cargo build --timings` deltas (Loro pulls ~40+ transitive crates via `loro-internal`: pest, im, num, serde_columnar, an LSM kv-store).
- **Decision gate:** Loro (best merge quality, cleanest API) vs `cola` (zero required deps, you keep the buffer, but YATA-class with documented prepend interleaving) vs self-written Fugue-lite. If footprint is the overriding constraint → `cola` or self-written.

### Spike-3 — diff→ops fidelity + determinism
Feed known interleaving cases (reverse-order shopping-list, concurrent prepend, two edits to the same line) through `update_by_line` + `import_batch` (or the chosen engine) and assert: each agent's block stays **contiguous** (no interleaving), output is **byte-stable**, and stable across **fixed-PeerID** assignment and import order.
- **Decision gate:** if reconstructed-diff ops lose Fugue's anti-interleaving property, either constrain to disjoint-only auto-merge (escalate ALL overlaps) or pick a different engine.

---

## 10. Library comparison (for Spike-2 reference)

| | **Loro** | **cola** | **diamond-types** | **yrs** | **Automerge** |
|---|---|---|---|---|---|
| Algorithm | **Fugue** (proven maximal non-interleaving) | custom G-tree, **YATA-class** | **eg-walker** (proven maximal, EuroSys'25) | YATA | RGA (weakest) |
| Code interleaving rank | **Tier 1** | Tier 2 | **Tier 1** | Tier 2 | Tier 3 |
| Maintenance | 1.13.6 (2026-06-21) | 0.5.1 (2025-07) | **no release** (crates.io frozen 2022; git master) | 0.27.x (2026-06) | 0.10.0 (2026-06) |
| Stable 1.0 | **yes** | no | git-pin only | no (pre-1.0) | no |
| License | MIT | MIT | ISC | MIT | MIT |
| Footprint | **heaviest** (~40+ transitive) | **lightest (zero required dep)** | medium (pins `smallvec` 2.0-**alpha**) | light–medium (~10) | medium–heavy (~16; sync/storage unused) |
| final-file→ops built in | **yes** (`update`/`update_by_line`) | no (self diff+replay) | no | **no** (diff vs StateVector) | yes (`update_text`) |
| N-way commutative | **yes** (`import_batch`) | yes (self-driven) | yes | yes | yes |
| Model-A fit | **strong** | strong (best footprint) | strong algo but git-only | medium | strong but low-level + bundles sync/storage |

**Why not the others:** diamond-types' eg-walker is conceptually closest (replay only divergent regions = exactly base+N-diffs) but is **frozen on crates.io (2022), API in flux, pins an alpha smallvec** → effectively an unstable git pin. yrs is the most-used but pre-1.0, has no text→ops diff, highest integration tax. Automerge's Rust API is officially "low-level, under-documented" and bundles unused sync/columnar/compression. **DeltaDB** (Zed) is excluded — a closed-waitlist product, not a published crate; cited only as prior art that human+AI co-editing of one file wants deterministic convergence.

**Naming trap (decision-critical):** the maintained crate is **`cola` v0.5.x**, **not** `cola-crdt` 0.1.1 (a stale 2023 alias from the same repo).

---

## 11. Risks & open questions

- **[Highest] Overlap rate is the whole premise.** If rare or N≈2, base-pinned structural merge + the same gate captures ~all the value with **zero new deps**. → Spike-1 decides whether CRDT is worth doing.
- **diff→ops fidelity (Model-A-specific unknown).** Synthesized origins ≠ live-edit origins; may erase Fugue's anti-interleaving benefit. → Spike-3.
- **Footprint vs zero-dep discipline (the one surviving objection to Loro).** ~40+ transitive crates; binary/compile-time delta unmeasured. → Spike-2.
- **Loro-vs-cola tie-break is explicit:** if footprint is the overriding constraint → `cola` (zero deps, you write the diff+replay driver); otherwise → Loro (proven Fugue, built-in final-file→ops, stable 1.0).
- **Novelty risk:** no known agent system uses final-state-diff→CRDT. Ship behind a flag, beside proven reconcilers, default off.

---

## 12. Bottom line

Build P3 as **Model A (post-hoc, isolated agents) with a swappable `ConcurrentMerger` and a layered cargo-check/test/escalate gate** — but **measure overlap first (Spike-1)**. Loro is the engine if we proceed and footprint is acceptable; `cola` or a self-written Fugue-lite is the zero-dependency fallback. Auto-merge only disjoint hunks on green gates; escalate every overlap to the existing judge+cherry path. Convergence, never correctness.
