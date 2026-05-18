# ADR 0001 — E1 Versioned-Store Spike Findings

**Status**: Accepted (2026-05-18)
**Spec affected**: `plans/2026-05-18-continual-learning-versioned-evolution.md` §4
**Spike branch**: `spike/e1-versioned-store` (frozen, tagged `spike-e1-final`)
**Spike crate**: `spike-e1-versioned-store/` (workspace-excluded after this ADR)

## Context

Before committing 3 weeks of W1-W3 implementation to E1 (dual git-repo versioned store under `~/.mur/`), ran a 2-day spike validating 8 risk assumptions. CI matrix (ubuntu / macos-arm / windows) ran 5 risks. 3 went green by inspection; 2 surfaced production-blocking findings.

| Risk | Status | Notes |
|---|---|---|
| R1 git2 three-platform | ✅ green | smoke + R1 pass on ubuntu / macos / windows |
| R4 external `git reset --hard` recovery | ✅ green | `detect_external_change()` + `rebuild_index()` recover cleanly |
| R8 split-brain (one .git nuked) | ✅ green | `repair_agents()` static fn re-inits, knowledge layer untouched |
| **R2 history() perf @ 1k patterns** | ❌ → **FIN-1/2/3** | live `history()` took 1.94s (target 100ms, 20×) |
| **R5 telemetry growth @ 24h appends** | ✅ (after fix) | exposed libgit2 quirk on `*/foo/` ignore patterns |
| R3 / R6 / R7 | not run | judged non-blocking for §4 design decision; deferred to W1-W3 |

## Findings

### FIN-1: save hot path must NEVER call `git index.add_all`

**Evidence.** CI macos / ubuntu / windows all timed out at 25min during R2's 3000-commit seed phase. Root cause: `commit_paths()` called `index.add_all(["."], ...)` on every commit, walking the entire working tree per commit. With 3000 patterns accumulating in the tree, total work was O(N²) ≈ 4.5M filesystem operations. Removed in spike commit `<TBD-sha>`.

**Rule.** Production `VersionedYamlStore` / `VersionedAgentStore` MUST stage only explicit paths passed by the caller. No `add_all` on the hot path. Recovery paths (e.g. `repair_agents` for split-brain) may use `add_all` since they run once per disaster.

**Verification.** After fix, smoke + R1 + R4 + R8 all complete in < 1 second on all three OS.

### FIN-2: version derivation must be O(1)

**Evidence.** Even after FIN-1 fix, R2 still hung. Root cause: `save_pattern()` called `current_pattern_version()` which called `history()` (full git revwalk + per-commit tree diff). Each save walked the entire history-to-date; cumulative work was again O(N²) ≈ 4.5M tree diffs.

**Rule.** Version derivation on the save hot path MUST be O(1) regardless of repo size. Spike now uses `archive/patterns/<name>/` directory count plus current-file existence — a single `read_dir` call. Production implementation may store version inline in pattern YAML or in a sidecar metadata file, but MUST NOT walk git history during save.

**Verification.** After fix, R2 completed in ~19 seconds end-to-end (seed + history measurement). Seed phase was ~17s, leaving the actual measurement of interest visible.

### FIN-3: `history()` must read from a cache, NOT walk git log live

**Evidence.** With FIN-1 + FIN-2 fixes in place, R2 finally completed and measured `history("p0500")` on a 3000-commit repo: **1.94 seconds**. The 100ms target was set in §4.2.6 as the limit beyond which `mur pattern history` and GUI History panel become unusable. 1.94s is 20× over.

The walk is O(N) commits × O(diff) per commit. At 3000 commits with libgit2 in release mode, ~640µs per commit-touch check. There is no obvious algorithmic speedup of the live walk; the only way to hit < 100ms is a per-pattern history index that mur maintains alongside writes.

**Rule.** `~/.mur/.mur-versions.yaml` (and the corresponding file under `agents/`) is upgraded from a **diagnostic drift detector** to a **load-bearing per-pattern history index**. Schema:

```yaml
schema_version: 3
knowledge_head: <12-char sha>
agents_head: <12-char sha>
patterns:
  rust-error-handling:
    versions:
      - { v: 1, sha: abc123def456, ts: 2026-04-12T..., reason: "init" }
      - { v: 2, sha: def456abc789, ts: 2026-04-15T..., reason: "...refine" }
    current_version: 2
  ...
workflows:
  ...
```

Index is maintained on every save: O(1) append per save. Index is rebuilt from git log on `mur internals rebuild-index` (slow, runs only on recovery or first migration).

`mur pattern history <name>` reads from index directly. Live git log walk becomes a fallback only when index is missing or stale relative to HEAD.

**Verification target.** With the index, `history()` on a 10k-pattern × 5-revision repo should return in < 50ms. To be tested in W2.

### FIN-4: `.gitignore` patterns — use bare names, not `*/` anchors

**Evidence.** Initial `agents/.gitignore` used `*/telemetry/` (intent: match `agents/<name>/telemetry/`). libgit2's `Repository::statuses()` reported `agent-a/telemetry/` as untracked despite the pattern matching by git CLI semantics. Switching to bare `telemetry/` (no anchor) fixed the issue.

Separately discovered that `Repository::is_path_ignored()` consults the libgit2 ignore engine **directly** and is the authoritative answer — `statuses()` iteration has its own quirks around untracked-directory reporting.

**Rule.** Production `.gitignore` files under `~/.mur/` and `~/.mur/agents/` MUST use bare patterns:

```
# ~/.mur/agents/.gitignore — production
telemetry/
outbox-ledger
inbox/
crashlogs/
running.lock
.extract_digest
.apply-staging/
.apply-in-progress
```

(No `*/` prefix, no leading slash unless explicitly meant to anchor to gitignore-file's directory.)

**Test rule.** Tests that verify gitignore correctness MUST use `repo.is_path_ignored("<path>")` as the assertion, never `statuses()` iteration.

## Decision

Adopt FIN-1 through FIN-4 as mandatory rules in v2 spec §4. Patch the spec to:

1. Add §4.2.8 "Mandatory implementation rules from 2026-05-18 spike" enumerating FIN-1/2/3/4.
2. Upgrade `.mur-versions.yaml` semantics in §4.2.7 from "drift detector" to "load-bearing per-pattern index".
3. Replace `*/foo/` patterns with bare patterns in the §4.2.1 .gitignore examples.
4. Extend §4.3 acceptance criteria with one new check:
   - [ ] `history()` on 1000-pattern × 3-revision repo returns in < 100ms (via index, not live git log walk).

Spec patch lands in the same commit as this ADR.

## Consequences

### On scope of W1-W3

- Index maintenance is now in-scope for W1, not deferred. Adds ~3-5 days of work to W1.
- Acceptance criteria for W2 grows by one perf test.
- No change to overall dual-git-repo design or to E2-E6 dependencies.

### On performance characteristics

- `save_pattern` worst-case is O(1) git commit + O(1) index update + O(1) version derivation. Predictable regardless of repo size.
- `history()` worst-case is O(K) where K = revisions of THIS pattern, not total repo. Sub-100ms even for canonical 50-version patterns.
- `mur internals rebuild-index` is the only O(total-commits) operation; runs offline / on recovery only.

### On future maintenance

- Index file is now critical state. Backup / restore docs must include it explicitly.
- Index drift (file out of sync with git HEAD) is a new failure mode. Detection logic in §4.2.7 step 2 already covers it; recovery path is the rebuild command above.

## Evidence

| Artifact | Pointer |
|---|---|
| Spike crate | `spike-e1-versioned-store/` on branch `spike/e1-versioned-store` |
| FIN-1 fix commit | drops `add_all` from `commit_paths` |
| FIN-2 fix commit | switches `current_pattern_version` to archive-dir count |
| FIN-4 fix commit | `agents.gitignore` bare patterns + R5 test uses `is_path_ignored` |
| R2 measurement | CI log `2026-05-18T08:..` — `r2: history p0500 took 1.935411254s` |
| R5 measurement | CI log — `r5: telemetry file = 4 MB raw; agents/.git delta ~0` |
| Final green CI | `gh run list --workflow=spike-e1.yml` first all-green run after this ADR |

## References

- `plans/2026-05-18-continual-learning-versioned-evolution.md` §4 (E1 spec, this ADR patches)
- `docs/superpowers/specs/2026-05-18-mur-agent-manifest-design.md` (AgentManifest — uses execution-layer repo from §4.2.1)
- `docs/superpowers/specs/2026-05-18-commander-feedback-wire-protocol-design.md` (E5 — depends on knowledge-layer commit history from §4.2.5)
- `spike-e1-versioned-store/README.md` (spike intent + Day 1-3 plan)
- libgit2 docs on `git_status_options` and `git_ignore_path_is_ignored`
