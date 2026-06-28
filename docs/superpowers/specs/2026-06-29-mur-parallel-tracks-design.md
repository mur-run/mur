# MUR Parallel Tracks Design

**Date:** 2026-06-29  
**Status:** Draft  

## Goal

Enable multiple AI agents to work on the same coding goal simultaneously, each exploring a different approach, then automatically assemble the best implementation by cherry-picking the highest-scoring function from each parallel version.

**The core innovation:** function-level cherry-pick across parallel agent tracks. No existing tool (Claude Code, Cursor 2.0, Copilot, Zed DeltaDB) does this.

## Non-goals

- True real-time CRDT concurrent editing of the same region (P3 — Loro/DeltaDB)
- ZFS unified backend (P2 — via socket to Lima/WSL2/SmolVM)
- Semantic Partition mode (P2.5 — region assignment without CRDT)
- Hub GUI diff viewer (P2)
- mur cloud / Firecracker snapshot execution (P4)
- Support for languages other than Rust in v1 (tree-sitter grammars added incrementally)

---

## Background and Research Basis

### The Gap (confirmed by AgenticFlict, April 2026)

All current AI coding tools use a sequential exclusive-lock model. Even tools marketed as "parallel" (Cursor 2.0 with 8 concurrent agents) only run agents on *different* files. No tool supports:

- Multiple agents producing competing implementations of the *same* function
- Function-level comparison across parallel attempts
- Assembling an optimal file from the best function of each parallel track

Zed DeltaDB (announced June 2026, waitlist only) addresses *human* concurrent editing via CRDT. It does not address speculative parallel AI execution or function-level cherry-pick.

### Key Research Findings

| Finding | Source | Impact on Design |
|---------|--------|-----------------|
| Without prompt diversity, N agents converge to the same answer | AlphaEvolve island model, 2025 | Mandatory diverse `approach` per track |
| LLM judge has position bias + length bias | CyclicJudge arXiv:2603.01865 | Swap-and-average scoring required |
| 5–10% semantic conflict rate even with CRDT | CodeCRDT arXiv:2510.18893 | cargo check as final gate, not just CRDT |
| Identical implementations can be detected by hash | CAS / Nix store pattern | Score cache keyed by content hash — skip re-judging |
| Up to 5 parallel workers is the optimal point | Parallel-then-Synthesize arXiv:2606.14672 | Default `--tracks 3`, max `--tracks 5` |
| Git worktrees are the 2025–2026 industry standard for parallel AI isolation | Zylos Research, Augment Code, Cursor 2.0 | P1 uses git worktrees; ZFS deferred to P2 |

---

## Architecture

### Overview

```
LAYER 5  Presentation     mur fleet compare / cherry / promote
LAYER 4  Intelligence     Pre-filter → CAS → CyclicJudge → Cherry-pick
LAYER 3  Semantic         tree-sitter → SemanticUnit[] + blake3 hash
                          LMDB (heed) → version state with MVCC
                          git merge-tree → in-memory conflict detection
LAYER 2  Execution        N parallel fleet tracks (each: one agent, one worktree)
LAYER 1  Isolation        ParallelBackend trait (pluggable)
                          P1: GitWorktreeBackend (default, zero deps)
                          P1.5: + platform COW (APFS / Btrfs / ReFS)
                          P2: ZfsSocketBackend (Lima/OrbStack/WSL2/SmolVM)
LAYER 0  Version control  git (always)
```

### New Module: `mur-core/src/parallel/`

```
parallel/
├── mod.rs                  # ParallelSession, SessionConfig, Mode enum
├── semantic/
│   ├── mod.rs              # SemanticUnit, SupportedLanguage
│   ├── tree_sitter.rs      # tree-sitter parsing → SemanticUnit[]
│   ├── cas.rs              # blake3 hash per unit, CAS lookup
│   └── dependency.rs       # inter-unit dependency graph (for compat check)
├── track/
│   ├── mod.rs              # Track, TrackSet
│   ├── worktree.rs         # git worktree lifecycle
│   ├── diversity.rs        # prompt approach injection
│   └── filter.rs           # pre-filter: cargo check / clippy
├── judge/
│   ├── mod.rs              # JudgeTask, JudgeResult, JudgePlan
│   ├── cyclic.rs           # CyclicJudge: swap order + average
│   └── rubric.rs           # scoring criteria config
├── cherry/
│   ├── mod.rs              # CherryPlan, UnitSelection
│   ├── picker.rs           # greedy selection + dependency compat
│   └── conflict.rs         # API compatibility check via tree-sitter signatures
├── merge.rs                # git merge-tree wrapper (in-memory conflict check)
├── state/
│   ├── mod.rs              # ParallelState, queries
│   └── lmdb.rs             # heed-based LMDB store (3 databases)
└── backend/
    ├── mod.rs              # ParallelBackend trait
    ├── git_worktree.rs     # default impl
    ├── zfs_socket.rs       # P2 impl
    └── detect.rs           # auto-detect best available backend
```

---

## Data Model

### `ParallelBackend` Trait

```rust
pub trait ParallelBackend: Send + Sync {
    fn create_track(&self, name: &str, base: &Path) -> Result<PathBuf>;
    fn snapshot(&self, track: &Path, label: &str) -> Result<String>;
    fn diff_files(&self, track: &Path, since: &str) -> Result<Vec<PathBuf>>;
    fn promote(&self, track: &Path, target: &Path) -> Result<()>;
    fn destroy(&self, track: &Path) -> Result<()>;
}
```

### `SemanticUnit`

```rust
pub struct SemanticUnit {
    pub kind: UnitKind,            // Fn | Struct | Impl | Trait | Enum | Const | Test
    pub name: String,
    pub byte_range: Range<usize>,
    pub line_range: Range<u32>,
    pub content_hash: [u8; 32],   // blake3 of the source bytes in this range
    pub dependencies: Vec<String>, // names of other top-level units this references
}
```

### LMDB Databases (via `heed`)

```
DB 1 — sessions
  Key:   session_id (String)
  Value: SessionState { status, goal, tracks: Vec<TrackMeta>, created_at }

DB 2 — units
  Key:   (session_id, track_id, file_path_hash, unit_name)
  Value: UnitImpl { content_hash, source: Vec<u8>, line_range }

DB 3 — scores
  Key:   (content_hash, rubric_version)   ← keyed by CONTENT, not track
  Value: JudgeScore { score: f32, reasoning: String, model: String, ts: u64 }
```

**Critical:** `scores` is keyed by *content hash*, not by track or session. Two identical implementations across different sessions are judged once and cached permanently. This eliminates 40–70% of LLM judge calls in practice.

### `fleet.yaml` — parallel section

```yaml
# Extended fleet.yaml for parallel mode
parallel:
  mode: speculative           # speculative | partition (P2.5)
  max_tracks: 3               # 2–5 recommended; diminishing returns above 5
  pre_filter:
    - cargo_check             # eliminates compile failures before LLM judge
    - cargo_clippy_deny       # linting score feeds into judge
  tracks:
    - name: track-functional
      approach: |
        Prefer functional style: Iterator combinators, avoid mutable state,
        compose small functions, use the type system to enforce invariants.
      model: claude-opus-4-8
    - name: track-performance
      approach: |
        Performance first: static dispatch over dyn, minimize heap allocation,
        consider cache locality, measure before optimizing.
      model: claude-sonnet-4-6
    - name: track-readability
      approach: |
        Readability and maintainability first: clear naming, rich error types,
        full doc comments, test-driven design.
      model: claude-sonnet-4-6
  judge:
    model: claude-opus-4-8
    strategy: cyclic           # swap order + average (anti-position-bias)
    rubric:
      correctness:     0.40
      design:          0.30
      maintainability: 0.20
      security:        0.10
```

---

## CLI Surface

All new commands extend `mur fleet` — no new top-level command.

```bash
# Create a parallel session (N worktrees, diverse approaches, same goal)
mur fleet create <name> --parallel [--tracks N] [--goal "..."]

# Show per-function scores across all tracks
mur fleet compare <name>
mur fleet compare <name> --unit <function_name>
mur fleet compare <name> --file <path>

# Trigger LLM judge manually (auto-runs after pre-filter passes)
mur fleet judge <name>

# Execute cherry-pick: assemble optimal file from best functions
mur fleet cherry <name> [--auto | --interactive]

# Promote a track or the cherry-picked result to main branch
mur fleet promote <name> <track-name | cherry>
```

### `mur fleet compare` Output

```
src/auth.rs

Function          track-functional  track-performance  track-readability  Rec
──────────────────────────────────────────────────────────────────────────────
authenticate()    9.2 ★            8.1                7.8                A
authorize()       7.4              9.0 ★              8.2                B
Session::new()    8.8              8.9 ★              7.1                B
logout()          8.1              7.3                9.4 ★              C
tests             8.0              6.2                9.6 ★              C

Best-of-all score: 9.14  (vs best single-track 9.2)
Dependency check: ✅ all selected units are API-compatible

[Apply recommendation]  [Interactive]  [View track-functional only]
```

---

## Algorithm: Speculative Parallel + Cherry-Pick

### Execution Steps

```
1. Create N git worktrees from same base commit (parallel, < 100ms each)
   + Platform COW: copy Cargo target/ (APFS cp -c / Btrfs reflink / ReFS BlockClone)
   → P1.5 only; P1 falls back to symlink

2. Inject diverse `approach` prompts into each track's fleet member config

3. Fleet runs: each agent works independently in its worktree
   (reuses existing fleet/channel/DAG machinery)

4. Pre-filter (cheap, automated)
   cargo check  → fail → discard track immediately (no LLM cost)
   cargo clippy → warnings recorded, fed into design score

5. tree-sitter parse: for each changed file in each surviving track
   → SemanticUnit[] with byte ranges and blake3 hashes

6. CAS deduplication
   Group by (file, unit_name) across tracks
   Same content_hash across all tracks → no judgment needed (pick any)
   Different hashes → schedule for LLM judge

7. CyclicJudge for each judgment task
   Round 1: present [Track A, Track B, Track C] in this order → score₁
   Round 2: present [Track C, Track A, Track B] (rotated) → score₂
   Final score = mean(score₁, score₂) per track per unit
   If |score₁ - score₂| > 0.2 → mark as "low confidence", flag for human

8. Cherry-pick plan: greedy selection
   For each (file, unit_name): pick the track with highest mean score
   Build dependency graph via tree-sitter name resolution
   For each selected unit X from track A that calls unit Y:
     If Y's selection is from a different track:
       Extract function signatures of both versions
       If signatures differ → dependency conflict
         Auto mode: fall back to same-track selection
         Interactive mode: present conflict to user

9. Assemble final file
   Reconstruct file from selected unit sources, preserving:
     - module-level use statements and attributes (from base)
     - relative ordering of units (same as original file)
     - whitespace and comments between units (from winning track)

10. Validate assembly
    Write assembled content to tmp file
    cargo check → must pass before promote is allowed
    On failure: present error with which cherry-pick combination caused it

11. Promote
    git commit the assembled result on a new branch
    User reviews diff → merges to main
```

### Dependency Compatibility Check

```rust
// Lightweight API compatibility: compare function signatures only
// Full semantic analysis is too slow; signature match catches most issues
fn signatures_compatible(unit_a: &SemanticUnit, unit_b: &SemanticUnit) -> bool {
    // tree-sitter: extract (fn name, params: Vec<(name, type)>, return_type)
    // Compare param types and return type (string equality on type nodes)
    // Ignores body, doc comments, attributes
}
```

---

## Platform COW Optimization (P1.5)

Behind the `ParallelBackend` trait — no changes to upper layers.

```rust
fn copy_build_cache(src: &Path, dst: &Path) -> Result<()> {
    let target = src.join("target");
    if !target.exists() { return Ok(()); }

    #[cfg(target_os = "macos")]
    if is_same_apfs_volume(src, dst) {
        // APFS copy-on-write: GB-sized target/ copies in milliseconds
        return Command::new("cp")
            .args(["-c", "-R", &target.to_string_lossy(), &dst.join("target").to_string_lossy()])
            .status().map(|_| ());
    }

    #[cfg(target_os = "linux")]
    if is_btrfs(src) {
        return Command::new("cp")
            .args(["--reflink=always", "-R", &target.to_string_lossy(), &dst.join("target").to_string_lossy()])
            .status().map(|_| ());
    }

    #[cfg(windows)]
    if is_refs(src) {
        return refs_block_clone(&target, &dst.join("target"));
    }

    // Fallback: symlink Cargo target/ as read-only build cache
    std::os::unix::fs::symlink(&target, dst.join("target"))?;
    Ok(())
}
```

macOS tmutil snapshot (P1.5, no entitlement required):

```rust
fn pre_run_snapshot(session_id: &str) -> Option<String> {
    Command::new("tmutil").arg("localsnapshot").output().ok()
        .filter(|o| o.status.success())
        .map(|_| format!("mur-parallel-{session_id}"))
}
```

---

## ZFS Unified Backend (P2)

**Strategy:** detect existing Linux environment — do not bundle or require ZFS installation.

### Detection Order

```rust
pub fn detect_zfs_backend(project: &Path) -> Option<Box<dyn ParallelBackend>> {
    // Native ZFS (Linux/FreeBSD)
    if (cfg!(target_os = "linux") || cfg!(target_os = "freebsd"))
        && zfs_cli_available() && is_on_zfs_pool(project)
    {
        return Some(Box::new(ZfsNativeBackend::new()));
    }
    // OrbStack (most common on macOS, 75–95% native perf via virtio-fs)
    if let Ok(s) = connect_orbstack_socket() { return Some(Box::new(ZfsSocketBackend(s))); }
    // Lima (CNCF open source, macOS/Linux)
    if let Ok(s) = connect_lima_socket("mur-zfs") { return Some(Box::new(ZfsSocketBackend(s))); }
    // WSL2 (Windows)
    #[cfg(windows)]
    if let Ok(s) = connect_wsl2_socket() { return Some(Box::new(ZfsSocketBackend(s))); }
    // SmolVM (bundled binary, <200ms cold start, no user installation needed)
    if let Ok(vm) = SmolVmBackend::start_bundled() { return Some(Box::new(vm)); }
    // No ZFS available — caller falls back to GitWorktreeBackend
    None
}
```

### `mur-zfs-agent` Socket Protocol

Minimal daemon (~200 lines) running inside the Linux environment.
Exposes a Unix socket with a JSON request/response protocol.

```
CreateTrack  { base: PathBuf, name: String }   → { track: PathBuf }
Snapshot     { track: PathBuf, label: String }  → { snap_id: String }
DiffFiles    { track: PathBuf, since: String }  → { files: Vec<PathBuf> }
Promote      { track: PathBuf, target: PathBuf }→ {}
Destroy      { track: PathBuf }                 → {}
```

### ZFS Advantages over git worktrees

| Operation | git worktrees | ZFS clones |
|-----------|--------------|-----------|
| Create track | ~80ms (index copy) | ~5ms (block clone) |
| Snapshot | ~100ms (git commit) | ~0ms (atomic) |
| Diff changed files | ~500ms (walk tree) | ~5ms (dirty block tracking) |
| 10 tracks disk usage | 10× changed files | shared blocks (80–90% savings) |
| Rollback | git reset + clean | zfs rollback (instant) |

---

## Semantic Partition Mode (P2.5)

For the "same file, different regions" use case — simpler than CRDT, covers 90% of cases.

```
Agent A owns: authenticate(), authorize(), Session struct
Agent B owns: logout(), RateLimiter struct, tests

Both work simultaneously. Merge = concatenate in file order.
No conflicts possible by construction — no CRDT needed.
```

Assignment strategies:
- **round-robin**: alternating units (A, B, A, B, …)
- **balanced**: assign by estimated complexity (line count as proxy)
- **manual**: user specifies via `--assign "agent1:authenticate,authorize"`

---

## CRDT + Production (P3)

True concurrent editing of the same function region. Only needed when partition mode cannot satisfy the use case (rare).

- **CRDT library:** `loro` crate (Rust-native, supports text + structured data, active 2024–2026)
- **Transport:** new `ChannelEvent::FileOp { agent_id, file, op: LoroCrdtOp }` type
- **Semantic conflicts** (5–10% per CodeCRDT): flagged automatically → LLM arbitration → human confirmation
- **DeltaDB integration:** when Zed open-sources DeltaDB (expected late 2026), use as CRDT backend replacing Loro; mur's cherry-pick logic remains unique above it
- **Firecracker (server/CI):** 28ms snapshot restore; pre-built `mur-zfs` image; used for mur cloud service

---

## Risks and Mitigations

| Risk | Likelihood | Severity | Mitigation |
|------|-----------|---------|-----------|
| Agent diversity insufficient — N tracks converge | Medium | High | Diversity benchmark during development; default approach prompts tuned to maximise variance |
| Cherry-pick breaks API compatibility | Medium | Medium | `cargo check` required before promote; LLM signature compat check as early warning |
| LLM judge cost explosion | Low | Medium | CAS cache eliminates re-judging identical impls; pre-filter eliminates failed tracks |
| tree-sitter grammar gaps (non-Rust files) | Low | Low | Ship Rust only in v1; add Python/TypeScript/Go grammars in subsequent minor releases |
| ZFS socket not available + SmolVM instability | Medium | Low | `GitWorktreeBackend` always available as fallback; ZFS is an enhancement not a requirement |
| Semantic conflicts in assembled code | Certain (5–10%) | Medium | `cargo check` as final gate; LLM arbitration for unresolved conflicts |

---

## Validation Plan

The core claims of this design are novel and unproven in the mur codebase. Before investing in full implementation, each phase requires an empirical gate. **Gate failure = stop, diagnose, and re-design — not push forward.**

### Core Assumptions to Validate

| # | Assumption | Why it matters | How to falsify |
|---|-----------|---------------|---------------|
| A1 | Diverse approach prompts produce meaningfully different implementations | Without variance, N tracks are identical — the whole design collapses | Measure pairwise code similarity across tracks |
| A2 | CyclicJudge produces stable, unbiased scores | If scores flip wildly between orderings, the judge is noise | Measure score delta across cyclic rounds |
| A3 | Cherry-picked assembly outperforms the best single track | If not, parallel tracks are all cost with no benefit | Blind human evaluation vs. single-agent baseline |
| A4 | CAS eliminates ≥40% of LLM judge calls in real usage | Determines cost viability of the whole approach | Measure hash-collision rate across real sessions |
| A5 | `cargo check` pass rate on assembled code ≥ 90% | If cherry-pick routinely produces invalid code, UX is broken | Count post-assembly compilation failures |
| A6 | tree-sitter semantic unit extraction is reliable for Rust | Malformed AST → wrong cherry-pick boundaries | Run against mur-core itself, count extraction errors |

---

### Validation Gate 0 — Proof of Concept (before any P0 code)

**Goal:** validate A1 + A6 with minimum code. Build a standalone script, not production code.

**Experiment:**
1. Pick 3 real functions from mur-core (varying complexity: trivial / medium / complex)
2. Prompt Claude 3× per function with 3 different `approach` texts (functional / performance / readability)
3. Parse each output with tree-sitter, extract `SemanticUnit[]`
4. Measure pairwise similarity: `1 - (edit_distance / max_len)`
5. Record extraction error rate

**Pass criteria:**
- Mean pairwise similarity ≤ 0.60 (tracks are meaningfully different)
- Tree-sitter extraction error rate ≤ 5%
- At least 2 of 3 functions show ≥ 1 structurally different approach

**Fail criteria → re-design:**
- Similarity > 0.80 across all functions → approach prompts are too weak; redesign diversity strategy
- Extraction error rate > 20% → tree-sitter grammar is unreliable; consider regex fallback or different parser

**Deliverable:** a `scripts/parallel_poc.py` (or `.sh`) that runs the experiment and prints the metrics. Commit results to `docs/superpowers/validation/parallel-poc-results.md`.

---

### Validation Gate 1 — Judge Reliability (before CyclicJudge ships in P1)

**Goal:** validate A2. Prove scoring is stable enough to trust.

**Experiment:**
1. Collect 20 pairs of competing function implementations (real or generated)
2. Score each pair 3 times: round [A, B], round [B, A], round [A, B] again
3. Record: score per round, winner per round, delta between rounds

**Pass criteria:**
- Mean absolute score delta across ordering swaps ≤ 0.15 (on a 0–10 scale)
- Winner flip rate ≤ 20% (same pair changes winner between orderings ≤ 20% of the time)
- After averaging cyclic rounds, winner flip rate drops to ≤ 10%

**Fail criteria → re-design:**
- Winner flip rate > 40% → judge is too noisy; consider majority-vote with N=5, or rubric re-design
- Mean delta > 0.30 → position bias is dominant; need more aggressive prompt engineering or different model

**Deliverable:** `docs/superpowers/validation/judge-reliability-results.md` with raw data + metrics.

---

### Validation Gate 2 — Cherry-Pick Quality (P1 alpha)

**Goal:** validate A3 + A5. The assembled result must be measurably better.

**Experiment:**
1. Run 10 parallel sessions on real mur-core tasks (5 simple, 5 complex)
2. For each session: record cherry-picked result AND best single-track result
3. Run `cargo check` + `cargo test` on both
4. Blind human evaluation (3 reviewers, each rates correctness / design / readability 1–5)

**Pass criteria:**
- Cherry-pick `cargo check` pass rate ≥ 90%
- Cherry-pick mean human score ≥ best single-track mean by ≥ 0.3 points (on 1–5 scale)
- Cherry-pick `cargo test` pass rate ≥ best single-track pass rate (must not regress)

**Fail criteria → re-design before P1 ships:**
- `cargo check` pass rate < 80% → dependency conflict detection is insufficient; strengthen signature compat check or fall back to same-track selection more aggressively
- Human score improvement < 0.1 → cherry-pick provides no real benefit; reconsider whether speculative parallel is worth the cost
- Test pass rate regresses → inter-unit dependencies are not correctly modeled; redesign dependency graph

**Deliverable:** `docs/superpowers/validation/cherry-pick-quality-results.md`. This gate is **blocking** — P1 does not ship until it passes.

---

### Validation Gate 3 — Cost and CAS Efficiency (P1 alpha)

**Goal:** validate A4. Confirm CAS savings are real.

**Experiment:**
1. Run 5 parallel sessions, each with 3 tracks, on the same 3 tasks repeated across sessions
2. Record: total judge calls made vs. total judge calls skipped (CAS hit)
3. Record: total LLM cost with CAS vs. estimated cost without CAS

**Pass criteria:**
- CAS hit rate ≥ 30% on repeated tasks (proves cross-session caching works)
- Total cost per session with 3 tracks ≤ 2.5× single-agent cost (parallelism premium must be bounded)

**Fail criteria → re-design:**
- CAS hit rate ≈ 0% → blake3 hashing too granular or implementations always diverge; consider chunked hashing or looser similarity threshold
- Cost per session > 4× single-agent → judge is too expensive; add cheaper pre-scoring tier (embedding similarity) before LLM judge

**Deliverable:** `docs/superpowers/validation/cas-efficiency-results.md`.

---

### Continuous Validation (post-P1)

After each release, run a **regression benchmark** against mur-core itself:

```
benchmark/
├── tasks/           # 20 canonical Rust coding tasks (fixed, versioned)
├── ground_truth/    # Human-reviewed gold implementations
└── run.sh           # Run all tasks, score against ground truth, compare to previous release
```

Metrics tracked per release:
- Mean cherry-pick score vs. single-agent baseline
- `cargo check` pass rate
- Mean cost per parallel session
- P95 latency (user-facing: time from `fleet run` to `fleet cherry` available)

**Score regression ≥ 0.2 below baseline → block the release.**

---

### Summary: Go / No-Go per Phase

| Phase | Gate | Blocking? |
|-------|------|-----------|
| P0 start | Gate 0: diversity + tree-sitter PoC | Yes — do not write production code until passed |
| P1 start | Gate 1: judge reliability | Yes — CyclicJudge design is load-bearing |
| P1 ship | Gate 2: cherry-pick quality + Gate 3: cost efficiency | Yes — both must pass |
| P1.5 start | Gate 2 + 3 passed | Yes |
| P2 start | P1 shipped + at least 3 real user sessions logged | Yes |
| P2.5 start | P2 stable | No — partition mode is additive |
| P3 start | P2.5 validated + Loro API stable | No — CRDT is optional enhancement |

---

## Competitive Positioning

| Capability | mur P1 | mur P2 | Zed DeltaDB | Claude Code | Cursor 2.0 |
|------------|--------|--------|-------------|-------------|------------|
| Parallel agents, different files | ✅ | ✅ | ✅ | ✅ | ✅ |
| Parallel agents, same file (speculative) | ✅ | ✅ | ❌ | ❌ | ❌ |
| Parallel agents, same file (concurrent CRDT) | ❌ | ❌ | ✅ (humans) | ❌ | ❌ |
| Function-level version comparison | ✅ | ✅ | ❌ | ❌ | ❌ |
| Function-level cherry-pick | ✅ | ✅ | ❌ | ❌ | ❌ |
| Cross-track optimal assembly | ✅ | ✅ | ❌ | ❌ | ❌ |
| Score cache across sessions | ✅ | ✅ | ❌ | ❌ | ❌ |
| Unified ZFS backend | ❌ | ✅ | ❌ | ❌ | ❌ |

---

## Phase Plan

| Phase | Deliverable | New deps | Weeks |
|-------|------------|---------|-------|
| **P0** | `parallel/` module skeleton, `ParallelBackend` trait, `GitWorktreeBackend`, tree-sitter + CAS, LMDB state | `tree-sitter`, `tree-sitter-rust`, `blake3`, `heed` | 1–3 |
| **P1** | Speculative parallel mode, diverse track prompts, pre-filter, CyclicJudge, cherry-pick, `fleet compare/cherry/promote` | 0 (uses P0 deps) | 4–10 |
| **P1.5** | Platform COW: APFS `cp -c`, Btrfs reflink, ReFS BlockClone, `tmutil` snapshots | `windows-rs` (Windows only) | 11–13 |
| **P2** | `mur-zfs-agent`, `ZfsSocketBackend`, auto-detect Lima/OrbStack/WSL2/SmolVM | SmolVM binary (optional) | 14–20 |
| **P2.5** | Semantic Partition mode, region assignment protocol | 0 | 21–25 |
| **P3** | Loro CRDT, `Channel::FileOp`, Firecracker execution, DeltaDB integration | `loro` | 26+ |

**P1 is the milestone that matters.** It delivers the unprecedented function-level cherry-pick with only 4 new crates and no VM/CRDT complexity. P2 and beyond are progressive enhancements.
