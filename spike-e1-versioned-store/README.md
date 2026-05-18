# spike-e1-versioned-store

**Status:** throwaway spike — DELETE after decision meeting (day 3).
**Branch:** `spike/e1-versioned-store`
**Spec under test:** `plans/2026-05-18-continual-learning-versioned-evolution.md` v2 §4 (E1 dual git-repo).

## Why this exists

Before committing 3 weeks of W1-W3 work on E1, validate 8 risk assumptions
about the dual-git-repo design. Fail fast if any assumption is broken —
revising the spec is 100× cheaper than rewriting code.

## Day 1 — vertical slice (DONE)

`src/lib.rs` implements the 6 core ops on a dual-repo store:
1. `init()` — create knowledge + agents repos with .gitignores
2. `save_pattern()` — atomic write + archive + commit
3. `read_pattern()` — passthrough
4. `rollback_pattern()` — restore archived version as NEW commit
5. `history()` — git log filtered to one pattern's path
6. `detect_external_change()` / `rebuild_index()` — `.mur-versions.yaml` consistency

`tests/01_smoke.rs` — 7 happy-path assertions. Run:

```bash
cargo test -p spike-e1-versioned-store
```

If smoke fails: the spike is dead. Revisit §4 design before continuing.

## Day 2 — 8 risk tests

`tests/risk_stubs.rs` contains 8 stubs marked `#[ignore]`. Each has:
- **PASS criterion** — what "green" looks like
- **KILL criterion** — what would force §4 spec revision

| # | Risk | Effort | Kill criterion |
|---|------|--------|----------------|
| 1 | git2 on macOS/Linux/Windows | CI matrix (~1h setup) | Windows CI breaks → shell out to `git` |
| 2 | history() perf @ 1k patterns | 30 min | > 1s → cache becomes load-bearing |
| 3 | concurrent writers race | 1h | lost commits → need file locking |
| 4 | external `git reset --hard` recovery | 30 min | unrecoverable → need doctor cmd |
| 5 | telemetry growth control | 1h | agents/.git > 50MB / 24h → restructure |
| 6 | SIGKILL mid-commit torn write | 2h | torn writes → need WAL |
| 7 | migration of real ~/.mur | 1h | data loss → migration redesign |
| 8 | split-brain: one .git missing | 30 min | knowledge corrupted too → cross-repo coupling unsafe |

Un-ignore, fill `todo!()`, run, record finding.

## Day 3 — decision meeting (30 min)

Bring this README + risk_stubs.rs results. Three outcomes:

- **All green** → commit spike branch findings as ADR, delete spike crate,
  open W1-W3 sprint against v2 spec §4 unchanged.
- **1-2 yellow/red** → patch §4 spec for the specific failures, re-run only
  affected risk tests, then open W1-W3.
- **3+ red** → dual git-repo design has root issues. Re-open §4 from D7
  decision: consider single-repo with aggressive .gitignore, or per-agent
  bare git that lives outside `~/.mur/agents/`.

## What this spike intentionally does NOT do

- No CLI integration (no `mur pattern history`)
- No GUI integration
- No `mur-common` schema migration (no `version: u32` field on KnowledgeBase yet)
- No hooks / no sleep cycle integration
- No `mur-core` integration — workspace-excluded throwaway crate
- No production-quality error messages

These come in W1-W3 IF the spike passes.

## Removal

```bash
git checkout main
git branch -D spike/e1-versioned-store     # if findings are captured elsewhere
# OR
git checkout spike/e1-versioned-store
rm -rf spike-e1-versioned-store
# remove "spike-e1-versioned-store" from root Cargo.toml exclude
git commit -am "chore(spike): remove e1 spike after W1-W3 kickoff"
```
