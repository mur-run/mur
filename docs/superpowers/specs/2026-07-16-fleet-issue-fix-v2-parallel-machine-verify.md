# fleet-issue-fix v2 — parallel fan-out + machine-verifiable assertions

> Status: spec (not implemented) · 2026-07-16
> Replaces: v1 workflow `fleet-issue-fix` (single-assignee serial loop, human-verified diffs)
> Motivation: six issues took 5 parallel subagents with manual diff verification; a fleet-native version should match or beat this throughput with lower supervision cost

## 1. Problem

v1 fleet-issue-fix has two bottlenecks:

| Bottleneck | Symptom | Root cause |
|---|---|---|
| **Throughput** | N issues = N serial rounds | Single assignee, single worktree, no parallelism |
| **Supervision cost** | Supervisor reads every diff against spec to verify | Verification is human, not machine — every diff enters context |

## 2. Solution outline

Two new phases slot into the existing DAG:

```
recon (supervisor, unchanged)
  → fan-out
       ├── topology: classify each issue by file anchor → disjoint/overlapping groups
       ├── N worktrees (one per issue)
       └── N spec files (with machine-verifiable asserts, see §3)
  → parallel dispatch
       ├── parallel_jobs MCP: one rustsmith job per issue
       ├── each job is files-gated (disjoint files = no collision)
       └── overlapping issues in the same group are serial within that group
  → machine verify
       ├── per-issue: run asserts from spec (nextest exit code, grep needle count, cargo check)
       ├── two-strike rule preserved (same defect twice → supervisor takeover)
       └── supervisor only inspects a diff when a strike fires
  → gates (per worktree: check, fmt, clippy)
  → repomanager (one PR per issue)
  → merge (CI green → merge; standing authorization)
```

## 3. Machine-verifiable assertions in spec files

### 3.1 Format

The spec file (already a markdown artifact written into the worktree) gains a YAML frontmatter block with an `asserts:` list:

```yaml
---
issue: 712
slug: deny-own-profile-write
assignee: rustsmith
asserts:
  - type: test_exists
    filter: deny_own_profile
    crate: mur-agent-runtime
  - type: test_passes
    filter: policy
    crate: mur-agent-runtime
  - type: grep_count
    file: mur-agent-runtime/src/sandbox/policy.rs
    needle: SELF_PROTECTED_AGENT_FILES
    min: 1
  - type: cargo_check
    crate: mur-agent-runtime
  - type: no_grep
    file: mur-agent-runtime/Cargo.toml
    needle: "new-dependency"   # guard against implicit dep creep
---
```

### 3.2 Assertion types

| Type | Exit-zero when | Fails when |
|---|---|---|
| `test_exists` | At least one test name contains `filter` | No matching test found (spec asks for a test that wasn't written) |
| `test_passes` | `cargo nextest run -E 'test({filter})'` exits 0 | Test exists but fails (spec asks for correct behavior, got wrong behavior) |
| `grep_count` | Needle appears ≥ `min` times in `file` | Needle count below threshold (spec asks for a pattern, wasn't implemented) |
| `no_grep` | Needle is absent from `file` | Needle found (spec asks for something NOT to be present — dep removal, dead code) |
| `cargo_check` | `cargo check -p {crate}` exits 0 | Won't compile (catches the most common failure: patch doesn't apply cleanly) |
| `diff_lines` | `git diff --stat` shows ≥ `min` changed lines in `file` | Truncated/empty delivery caught before human inspection |

### 3.3 Verifier implementation

```bash
# verify-one-issue.sh — called from the workflow verify phase
SPEC=".worktrees/fix-$ISSUE/.task-$ISSUE-spec.md"
WORKTREE=".worktrees/fix-$ISSUE"

# Parse asserts from YAML frontmatter (yq or simple awk for the constrained format)
# For each assert, run the check; exit non-zero on first failure
# stdout: pass/fail per assert → workflow reads exit code
```

The workflow verify phase calls this once per issue. Exit code 0 = all asserts pass → proceed to gates. Exit code ≠ 0 → strike counter increments; two strikes on the same issue → supervisor inspects ONLY that issue's diff.

## 4. Fan-out topology

### 4.1 Classification

Before creating worktrees, the supervisor's recon output now includes a **file anchor list** per issue (already implicit — the recon step reads the issue and locates code anchors). The fan-out phase formalizes this:

```
issues = [
  {id:712, files:["mur-agent-runtime/src/sandbox/policy.rs", "mur-agent-runtime/src/tools/fs_policy.rs"]},
  {id:713, files:["mur-core/src/cmd/agent/cli/mod.rs", "mur-core/src/cmd/agent/cli/recover.rs"]},
  {id:715, files:["mur-agent-runtime/src/llm/mod.rs", "mur-agent-runtime/src/task_runner.rs"]},
  {id:716, files:["mur-agent-runtime/src/tools/suggest.rs", "mur-core/src/cmd/agent/cli/mod.rs"]},
  {id:717, files:["mur-common/src/skill/loader.rs", "mur-core/src/cmd/agent/skill.rs", ...]},
]

overlap_groups = group_by_overlap(issues)  // 713+716 share cli/mod.rs → same group
// → [{712}, {715}, {717}, {713, 716}]
// 4 groups, not 6 — 713+716 serial in their group, all groups run in parallel
```

### 4.2 Execution model

- **Between groups**: parallel (no file overlap → no merge conflict)
- **Within a group**: serial (shared files → second issue waits for first to commit)
- **Disjoint-groups guard**: `parallel_jobs` MCP's existing files-gate rejects a job if its file anchors overlap with any in-flight sibling — so classification bugs fail closed

## 5. Changes to the workflow script

### 5.1 New phases

```javascript
// fleet-issue-fix v2 phases — additive, not breaking

phase('fan-out')
const topology = classify(issues)  // → [{groupId, issues:[...], files:[...]}]
for (const group of topology) {
  group.worktrees = group.issues.map(i => 
    `git worktree add .worktrees/fix-${i.id} -b fix/${i.slug} origin/main`)
}

phase('spec')
// Write spec files WITH asserts frontmatter into each worktree

phase('dispatch')  
// parallel_jobs: one rustsmith job per issue, files-gated by topology

phase('verify')
// Machine: verify-one-issue.sh per worktree, exit-code gated
// Strike mechanism: same-issue two failures → supervisor takeover

// phases after verify: gates, commit, repomanager, merge — unchanged per-issue
```

### 5.2 Variables

Add to existing variables:
- `issues`: array of `{id, slug, files[], assignee}` — replaces single `issue`/`slug`/`assignee`

### 5.3 Backward compatibility

Single-issue mode preserves the existing variable interface:
- When `issues` is absent, fall back to `{issue}`, `{slug}`, `{assignee}` — identical to v1
- When `issues` is present, fan-out activates

## 6. What stays the same

- Spec-FILE handoff (path survives router compression)
- Write-first orders (no cargo/git/sed in dispatch)
- Conventional commit + repomanager PR
- CI-green → merge (standing authorization)
- Worktree isolation per issue
- Two-strike supervisor takeover (now gated on machine asserts, not human diff reading)

## 7. Risk: shared target directory

Parallel worktrees sharing one `target/` directory cause lock contention and spurious rebuilds. Mitigation options (decide at implementation):

**A. Per-worktree target (safe, disk-heavy):** `CARGO_TARGET_DIR=.worktrees/fix-<N>/target` — each issue gets its own build cache. ~2-5GB per worktree for mur-agent-runtime. Acceptable for ≤4 parallel groups.

**B. Serialized cargo (disk-light, slower):** Share the main checkout's target but serialize all `cargo check`/`cargo test` calls within a group. Groups already have disjoint files, so no rebuild churn.

**C. Hybrid:** shared target, serialized cargo per group (B), parallel across groups. This is the default recommendation — the parallelism win is in dispatch, not in compilation.

## 8. Implementation plan

1. **Spec asserts first** — machine-verifiable assertions work for single-issue v1 before fan-out exists. Build `verify-one-issue.sh` and prove it catches the three failure modes from the v1 campaign (empty delivery, truncated patch, wrong behavior). This immediately cuts v1 supervision cost.
2. **Topology classification** — `classify()` function: file-anchor sets → overlap groups. Pure data transform, testable without fleet.
3. **Fan-out phase** — wire `parallel_jobs` MCP, add the `issues` variable, parallel worktree creation.
4. **Dogfood** — run v2 on the next batch of issues; measure: wall-clock vs v1, supervisor context tokens consumed, missed-bug rate.

## 9. Success metrics

| Metric | v1 (serial, human verify) | v2 target (parallel, machine verify) |
|---|---|---|
| Wall-clock for 6 disjoint issues | N × (recon + dispatch + verify + gates) | max(dispatch per group) + gates |
| Supervisor diff-reading events | 1 per issue per strike | 0 (only on 2nd strike) |
| False-negatives (bug lands in main) | Near zero (human reads every diff) | Same (asserts encode the same checks) |
| False-positives (assert fails but fix is correct) | N/A | < 5% (primarily `grep_count` with brittle needles) |
