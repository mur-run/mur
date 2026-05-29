# Pattern → Skill Retirement Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement task-by-task. Every task ends on a **green checkpoint** (`cargo build --workspace` clean + `cargo test -p mur-core` no new failures + commit). Steps use checkbox (`- [ ]`) syntax.

**Status:** Phase 1+2 already shipped on `worktree-feat+pattern-to-skill-migration` (commit `7847f61`) — the **injection** pathway now loads skills from `~/.mur/skills/`, ranks them via the generic `Retrievable` scorer, and formats via `InjectedItem`. This plan covers the *retirement* tail.

---

## TL;DR — the reframe that drives this plan

"Delete Pattern" is **not one cleanup task**. Evidence-based mapping shows it is **three distinct efforts** with a hard dependency gate:

| Tier | What | Nature | Risk | Blocks Pattern-type removal? |
|---|---|---|---|---|
| **A** | Delete Pattern-only CLI surface + Pattern-only `capture/`+`evolve/` files | Mechanical cleanup | Low | No (independent) |
| **B** | **Build** skill-native fleet-sync: `/api/v1/skills`, `SignalTarget::Skill`, skill snapshot/drafts | **New feature** | High | **Yes** |
| **C** | Retire `skill from-pattern`, then remove the `Pattern` type from `scoring.rs` + `mur-common` | Type removal | Medium | — |

**The gate:** the `Pattern` type cannot be removed until **Tier B is built**, because `server/`, `sync/`, and `federation/` still move Patterns and **no skill equivalent exists yet** (verified: no `/api/v1/skills`, no `SignalTarget::Skill`, no `pull_skill_snapshot`). Tier B *is* the fleet-sync Phase 2 feature. So Pattern removal is gated on a feature build, not on cleanup.

**Recommended execution order:** Land Phase 1+2 → Tier A (safe, now) → (Tier B when fleet-sync Phase 2 is scheduled) → Tier C (finale). Tier A delivers real clutter reduction with zero feature dependency; Tier B/C are sequenced behind the fleet-sync feature.

---

## Why the last attempt produced 99+ errors

Bottom-up deletion of a **load-bearing type**: the `Pattern` struct was deleted while ~8 subsystems still referenced it. Correct approach = **Strangler Fig**: repoint/retire *users* of Pattern first, delete the type **last**, keep the tree compiling at every commit.

---

## Coupling map (verified — do NOT delete shared files)

### `capture/` — mixed
| File | Fate | Evidence |
|---|---|---|
| `capture/emergence.rs` | **KEEP (shared)** | `cmd/skill_suggest.rs:6`, `cmd/session.rs:90`, `server/sessions.rs:138`, `nudge/candidate.rs:1` |
| `capture/noise_filter.rs` | **KEEP (shared)** | `retrieve/gate.rs:262` |
| `capture/starter.rs` | **KEEP (shared)** | `cmd/context.rs`, `cmd/sync_cmd.rs` (language detection) |
| `capture/feedback.rs` | **KEEP (now shared)** | ⚠️ The *migrated* injection path reuses `InjectedPatternRecord`/`InjectionRecord`/`write_injection_record` (`inject/hook.rs:25`). Only the Pattern-CLI-specific `analyze_session_feedback` path is removable; record types stay. Rename "Pattern" → neutral is a *later* cosmetic task, not a delete. |
| `capture/curator.rs` | DELETE after A1 | only `cmd/session.rs:1369` (pattern curation) |
| `capture/import.rs` | DELETE after A1 | only `cmd/misc.rs:396`, `cmd/init.rs:899` |
| `capture/reflector.rs` | DELETE after A1 | only `capture/curator.rs:8` (internal) |

### `evolve/` — mixed
| File | Fate | Evidence |
|---|---|---|
| `evolve/skill_evolve.rs` | **KEEP (skill engine)** | `cmd/skill_evolve.rs:22` |
| `evolve/telemetry_reader.rs` | **KEEP (skill)** | `evolve/skill_evolve.rs:12` |
| `evolve/cooccurrence.rs` | **KEEP (live)** | ⚠️ migrated injection uses it: `record_cooccurrence_for_items` → `CooccurrenceMatrix` (`inject/hook.rs:26`); also `dashboard.rs`, `cmd/workflow.rs` |
| `evolve/feedback.rs` | **KEEP (generic)** | `gep.rs:7`, `context_api/mod.rs:12` |
| `evolve/decay.rs` | DELETE after A4 | `cmd/evolve_cmd.rs:57`, `cmd/sync_cmd.rs:918`, `cmd/misc.rs:136` |
| `evolve/maturity.rs` | DELETE after A4 | `cmd/evolve_cmd.rs:58`, `cmd/sync_cmd.rs:919`, `cmd/misc.rs:138`, `cmd/eval.rs:10` |
| `evolve/consolidate.rs` | DELETE after A4 | `cmd/evolve_cmd.rs:14` |
| `evolve/compose.rs` | DELETE after A4 | `cmd/evolve_cmd.rs:140`, `cmd/workflow.rs:640` |
| `evolve/linker.rs` | DELETE after A4 | `cmd/pattern.rs:158`, `cmd/learn.rs:153`, `cmd/misc.rs:218` |
| `evolve/lifecycle.rs` | DELETE after A4 | `cmd/misc.rs:137` |
| `evolve/decompose.rs` | DELETE after A4 | `cmd/workflow.rs:642` |
| `evolve/commander_bridge.rs` | DELETE after A4 | `cmd/misc.rs:187` |

> Note: several "deletable" evolve files have consumers in `cmd/sync_cmd.rs`, `cmd/workflow.rs`, `cmd/eval.rs`, `cmd/misc.rs` — those call sites must be removed/repointed **first** (part of A4), or those commands lose pattern functionality. Each is a compile-driven step.

### Pattern-reading paths that MUST survive (until Tier C)
- `cmd/skill_from_pattern.rs` — reads `~/.mur/patterns/` via `YamlStore`, converts Pattern→Skill (`cmd_from_pattern` dispatch `dispatch.rs:349-350`). **The one bridge that keeps `Pattern` + `YamlStore` read alive.**
- `cmd/misc.rs::cmd_stats` (`dispatch.rs:57`), `cmd_gc` (`dispatch.rs:69`) — read/modify pattern store.
- `cmd/pattern.rs::cmd_search` — called by unified search (`cmd/search.rs:148`).
- `cmd/context.rs`, `cmd/reindex.rs`, `cmd/session.rs` (push/reflect), `cmd/drafts.rs` — YamlStore reads/writes.
- `cmd/learn.rs::parse_llm_patterns` — used by `cmd/session.rs` LLM analysis (keep even though rest of learn.rs goes).

---

## Tech stack / conventions
- Rust 2024, cargo workspace, existing `mur-core` deps. No new dependencies for Tier A.
- mur-core favors few large modules. Deleting whole files is fine; do **not** split survivors.
- Green checkpoint after every task. Pre-existing baseline: **1404 pass, 6 fail** (conversations rollup + paths — unrelated; do not attribute to this work).

---

# TIER A — Delete Pattern-only CLI + Pattern-only modules (safe, do now)

Top-down per cluster: **dispatch branch → CLI enum variant → cmd fn/file → now-orphaned module**. Compile after each cluster.

## Task A1: Remove the pattern-authoring/lifecycle CLI cluster
Commands: `New`, `Pin`, `Mute`, `Boost`, `Promote`, `Deprecate`, `Links`, `Edit`, `Why`, `Feedback`, `Pattern{Show/History/Diff/Rollback}`.

**Files:**
- Modify: `dispatch.rs` — remove branches at lines 36, 59, 60, 61, 62-67, 119-125, 161, 162, 163, 297-300, 301.
- Modify: `cli/mod.rs` — remove `Commands` variants: `New`, `Feedback`, `Pin`, `Mute`, `Boost`, `Promote`, `Deprecate`, `Pattern`, `Links`, `Why`, `Edit` (keep `Stats`, `Gc`).
- Modify: `cli/actions.rs` — remove `FeedbackAction` (95-109), `PatternAction` (112-137).
- Delete: `cmd/pattern_history.rs` (all 3 fns dispatch-only).
- Modify: `cmd/pattern.rs` — remove `cmd_new`, `cmd_pattern_show`, `cmd_edit`, `cmd_promote`, `cmd_deprecate`, `cmd_boost`, `cmd_feedback`, `cmd_feedback_auto`, `cmd_set_lifecycle`, `cmd_links`. **KEEP `cmd_search`** (used by `cmd/search.rs`).
- Modify: `cmd/mod.rs` — drop `pub mod pattern_history;` if present.

**Steps:**
- [ ] Remove dispatch branches first (compile error will list the now-unused cmd fns — that's the worklist).
- [ ] Remove CLI enum variants + `clap` action enums.
- [ ] Delete `pattern_history.rs` + its `mod` decl.
- [ ] Trim `pattern.rs` to just `cmd_search` (+ helpers it needs). Watch `evolve::linker` use at `pattern.rs:158` — it goes with the deleted fns.
- [ ] **Green checkpoint.** Commit: `refactor(migrate): A1 remove pattern authoring/lifecycle CLI`.

## Task A2: Remove `learn` + `emerge`
Superseded by `skill generate` / `skill suggest`.

**Files:**
- Modify: `dispatch.rs` — remove `Learn` (71-84) and `Emerge` (189) branches.
- Modify: `cli/mod.rs` — remove `Learn` variant (56-60) and `Emerge` variant.
- Modify: `cli/actions.rs` — remove `LearnAction` (71-92).
- Modify: `cmd/learn.rs` — remove `cmd_learn_extract`, `cmd_learn_cross`, `cmd_emerge`. **KEEP `parse_llm_patterns`** (used by `cmd/session.rs:1365+`). If that leaves the file as a single helper, that's fine — keep the module.
- Watch: `cmd/learn.rs:153` uses `evolve::linker`, `:213` uses `capture::emergence` (emergence is a KEEP — only the call site in the deleted fns goes).

**Steps:**
- [ ] Remove dispatch + CLI variants.
- [ ] Trim `learn.rs` to `parse_llm_patterns` (+ its deps).
- [ ] **Green checkpoint.** Commit: `refactor(migrate): A2 remove learn/emerge CLI`.

## Task A3: Delete Pattern-only `capture/` files
After A1/A2 their only consumers (`session.rs::curate`, `misc::import`, `init`) must be trimmed first.

**Files:**
- Modify: `cmd/session.rs` — remove the `capture::curate()` call (`:1369`) and pattern-curation block.
- Modify: `cmd/misc.rs` — remove `capture::import` use (`:396`); `cmd/init.rs` — remove `:899` import call (or repoint to skill import if one exists — check first).
- Delete: `capture/curator.rs`, `capture/import.rs`, `capture/reflector.rs`.
- Modify: `capture/mod.rs` — drop the three `mod` decls.
- **Do NOT touch** `capture/emergence.rs`, `noise_filter.rs`, `starter.rs`, `feedback.rs`.

**Steps:**
- [ ] Remove call sites in `session.rs`, `misc.rs`, `init.rs`.
- [ ] Delete the 3 files + mod decls.
- [ ] **Green checkpoint.** Commit: `refactor(migrate): A3 delete pattern-only capture modules`.

## Task A4: Remove `evolve` + `gep` CLI and delete Pattern-only `evolve/` files
Superseded by `skill evolve/sweep/doctor/consolidate`.

**Files:**
- Modify: `dispatch.rs` — remove `Evolve` (164-183), `Gep` (185-188) branches.
- Modify: `cli/mod.rs` — remove `Evolve`, `Gep` variants; `cli/actions.rs` — remove `EvolveAction` (167-180), `GepAction` (159-164).
- Delete: `cmd/evolve_cmd.rs`, and the GEP fns in `cmd/community_cmd.rs` (`cmd_gep_evolve`, `cmd_gep_status` — check whether the rest of community_cmd is still used before deleting the whole file).
- Trim consumers of soon-deleted evolve files: `cmd/sync_cmd.rs:918-919` (decay/maturity), `cmd/workflow.rs:640-642` (compose/decompose/cooccurrence — **cooccurrence is KEEP**, only compose/decompose go), `cmd/eval.rs:10` (maturity), `cmd/misc.rs:136-138,187,218` (decay/maturity/lifecycle/commander_bridge/linker).
- Delete: `evolve/{decay,maturity,consolidate,compose,linker,lifecycle,decompose,commander_bridge}.rs`.
- Modify: `evolve/mod.rs` — drop those `mod` decls. **KEEP** `skill_evolve`, `telemetry_reader`, `cooccurrence`, `feedback`.

**Steps:**
- [ ] Remove dispatch + CLI variants.
- [ ] Trim each consumer call site (compile-driven).
- [ ] Delete the 8 files + mod decls.
- [ ] **Green checkpoint.** Commit: `refactor(migrate): A4 remove evolve/gep CLI + pattern-only evolve modules`.

**End of Tier A:** Pattern-authoring/evolution CLI is gone; Pattern type still exists (server/sync/from-pattern). `mur new/learn/emerge/evolve/pin/...` no longer exist — **update README + docs CLI surface** (see Doc task below).

---

# TIER B — Build skill-native fleet-sync (FEATURE — schedule with fleet-sync Phase 2)

> This is **net-new build**, not cleanup. Verified gaps: no `/api/v1/skills` routes, no `SignalTarget::Skill`, no `pull_skill_snapshot`, no skill drafts. Skills DO already have: file store (`mur_common/skill/store.rs`, `~/.mur/skills/`), vector index (`skill_index/` → shared LanceDB). **Coordinate with the fleet-sync Phase 2 plan** (`docs/superpowers/plans/2026-05-29-fleet-sync-pro-phase1.md` and successors) — this tier likely belongs *there*, not in a cleanup PR. Endpoints must use the `/api/v1/core/...` namespace (per project memory: fleet-sync is `/api/v1/core/fleet/`, NOT `/mur/`).

## Task B1: Skill HTTP API
- [ ] Create `server/skills.rs` mirroring `server/patterns.rs` CRUD (list/get/create/update/delete) over the skill store.
- [ ] Add `AppState::skill_store()` (`server/mod.rs:101-112`) + register routes in `build_router()` (`server/mod.rs:167-241`).
- [ ] Add `search_skills()` (`server/search.rs`), `get_skill_stats()` (`server/stats.rs`) using the generic `Retrievable` scorer.
- [ ] Extend context API (`server/context.rs`) to retrieve/ingest/feedback skills.
- [ ] Green checkpoint + integration tests.

## Task B2: Skill sync signals + inbox/outbox
- [ ] Add `SignalTarget::Skill { name, scope }` (and skill-draft proposal target) in `mur-common`.
- [ ] Extend `sync/inbox.rs::apply_one` (`:153-208`) with a Skill branch (skill evidence/telemetry update).
- [ ] Add skill sync endpoints to `sync/client.rs` (`/api/v1/core/skills/pending`, ack, publish).
- [ ] Green checkpoint + tests.

## Task B3: Skill federation snapshot
- [ ] Add `pull_skill_snapshot()` in `federation/snapshot.rs` (parallel to `pull_snapshot`), `SkillFilter`, cache at `~/.mur/agents/{name}/skills_cache/`.
- [ ] Green checkpoint + tests.

## Task B4: Delete Pattern server/sync once skill paths are live
- [ ] Delete `server/patterns.rs` + pattern routes (`server/mod.rs:183-201`).
- [ ] Remove Pattern branches from `sync/inbox.rs`, pattern draft APIs from `sync/client.rs`, `pull_snapshot` pattern path.
- [ ] Green checkpoint.

---

# TIER C — Remove the Pattern type (finale)

## Task C1: Retire `skill from-pattern`
- [ ] Decide policy (see Open Decision). Recommended: keep read-only + deprecation warning through one release; provide `mur skill from-pattern --all` one-shot migration; then delete.
- [ ] When retiring: remove `cmd/skill_from_pattern.rs`, dispatch `:349-350`, `SkillAction::FromPattern`.

## Task C2: Drop Pattern from the scorer
- [ ] Remove `impl Retrievable for Pattern` + `scope_mult`/`lang_mult`/`kind_score_boost` Pattern boosts + `pub type ScoredPattern = Scored<Pattern>` alias (`retrieve/scoring.rs`).
- [ ] Repoint remaining `ScoredPattern` users (`interactive.rs`, `server/search.rs`) — should be gone after Tier B.
- [ ] Green checkpoint.

## Task C3: Remove Pattern type from `mur-common`
- [ ] Move still-shared types out of `mur-common/src/pattern.rs` into `knowledge.rs` (the original "Phase 4").
- [ ] Delete the `Pattern` struct, `YamlStore` Pattern specialization (`store/yaml.rs`), `VersionedYamlStore::save_pattern`, MKEF pattern conversion (`store/exchange.rs`), pattern LanceDB rows.
- [ ] Delete now-orphaned `capture/feedback.rs` Pattern parts (or rename `InjectedPatternRecord`→`InjectedItemRecord`), `evolve/feedback.rs` if unused.
- [ ] Final green checkpoint. `grep -rn "Pattern" mur-core/src` should return only incidental/string matches.

---

## Documentation task (after Tier A, and again after C)
Per CLAUDE.md Documentation Checklist:
- [ ] `README.md` — remove retired commands from CLI surface.
- [ ] Docs site `mur-server/dashboard/docs-content/` + `coreNavigation.tsx` — same.
- [ ] `CLAUDE.md` CLI Surface section — drop `new/learn/emerge/evolve/...`.
- [ ] `mur verify --all` to catch stale doc claims.

---

## Open Decision (needs user sign-off before Tier C1)
**Fate of `skill from-pattern` + existing `~/.mur/patterns/`:** keep the read-only migration bridge through a deprecation window (recommended), or drop immediately and require users to re-author. This is the last thing pinning the `Pattern` type alive.

## Sequencing summary
1. **Land Phase 1+2** (PR) — already green; only behavior change is `mur why` → helpful stub.
2. **Tier A** (A1→A4) — independent, low-risk, do now. Each task = one green commit.
3. **Tier B** — schedule inside fleet-sync Phase 2 (feature work). Gates Pattern-type removal.
4. **Tier C** — finale after B + from-pattern retirement.
