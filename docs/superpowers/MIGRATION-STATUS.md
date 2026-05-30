# Pattern → Skill Migration Status

**Last updated:** 2026-05-30  
**Phase:** Tier A complete; Tier B/C gated on fleet-sync Phase 2

---

## Summary

The Pattern → Skill migration is a three-tier effort to retire the legacy Pattern type and promote Skills as the unified learning system. **Tier A (cleanup) is complete.** Tier B and C are blocked pending fleet-sync Phase 2 implementation.

| Tier | Work | Status | Blocker |
|---|---|---|---|
| **A** | Retire Pattern-only CLI + modules | ✅ **SHIPPED** | None |
| **B** | Build skill-native fleet-sync infrastructure | 🔲 Not started | Scheduled with fleet-sync Phase 2 |
| **C** | Remove Pattern type from codebase | 🔲 Not started | Tier B completion |

---

## Tier A — Complete ✅

**Shipped in PRs:** #313 (C2b dead code cleanup), #316 (C2b migration), #317 (A1-A4 retirement)  
**Commits:** 5 commits (docs + A1-A4 phases)

### What was removed:
- **A1**: Pattern authoring/lifecycle CLI (`mur new`, `mur edit`, `mur promote`, `mur deprecate`, `mur pin`, `mur mute`, `mur boost`, `mur links`, `mur feedback`, `mur why`)
- **A2**: Pattern learning CLI (`mur learn extract`, `mur emerge`)
- **A3**: Pattern-only capture modules (`capture/curator.rs`, `capture/import.rs`, `capture/reflector.rs`)
- **A4**: Pattern-only evolve modules (`evolve/{decay,maturity,consolidate,compose,linker,lifecycle,decompose,commander_bridge}.rs`) + GEP

### Impact:
- ✅ ~1600 lines of dead code removed
- ✅ Pattern authoring surface retired (users migrate to `skill generate` / `skill suggest`)
- ✅ README + CLI reference updated
- ✅ All tests passing, Clippy clean

### Remaining Pattern references (intentional):
- `cmd/skill_from_pattern.rs` — One-way migration bridge (Pattern → Skill)
- `YamlStore` Pattern reading — Used by skill import and federation
- `server/patterns.rs` — Fleet-sync pathway (blocks on Tier B)
- `Pattern` type in `mur-common` — Removed in Tier C

---

## Tier B — Blocked on fleet-sync Phase 2 🔲

**Estimated scope:** 4 tasks (B1-B4), net-new feature build  
**Dependencies:** fleet-sync Phase 2 design complete  
**Estimated timeline:** TBD with fleet-sync team

### What needs to be built:

**B1: Skill HTTP API** (`server/skills.rs`)
- CRUD routes: `/api/v1/core/skills/` (list, get, create, update, delete)
- Search: `/api/v1/core/skills/search`
- Stats: `/api/v1/core/skills/{name}/stats`
- Extend context API for skill retrieval/feedback

**B2: Skill sync signals + inbox/outbox**
- Add `SignalTarget::Skill` to sync protocol
- Extend `sync/inbox.rs` with skill evidence/telemetry update
- Add skill endpoints: `/api/v1/core/skills/pending`, ack, publish

**B3: Skill federation snapshot**
- `pull_skill_snapshot()` → cache at `~/.mur/agents/{name}/skills_cache/`
- `SkillFilter` for selective federation

**B4: Delete Pattern server/sync** (after B1-B3 live)
- Remove `server/patterns.rs` + routes
- Clean Pattern branches from `sync/inbox.rs`

### Why it's blocked:
The retirement plan explicitly notes: *"This tier likely belongs **there** [in fleet-sync Phase 2 plan], not in a cleanup PR."* Tier B is net-new feature work (building skill infrastructure), not cleanup. It belongs semantically and organizationally in the fleet-sync effort.

---

## Tier C — Depends on Tier B 🔲

**Estimated scope:** 3 tasks (C1-C3)  
**Dependencies:** Tier B completion (all Skill HTTP + sync routes live)  
**Estimated timeline:** After fleet-sync Phase 2 ships

### What needs to happen:

**C1: Retire `mur skill from-pattern`**
- Add deprecation warning
- Provide `mur skill from-pattern --all` one-shot migration
- Delete command in next release

**C2: Drop Pattern from scorer**
- Remove `impl Retrievable for Pattern`
- Remove Pattern-specific boosts (`scope_mult`, `lang_mult`, `kind_score_boost`)
- Remove `ScoredPattern` type alias

**C3: Remove Pattern type**
- Delete `mur-common/pattern.rs` + `Pattern` struct
- Move shared types (`KnowledgeBase`, etc.) to `knowledge.rs`
- Delete `YamlStore` Pattern specialization
- Remove `LanceDB` Pattern rows

### Verification:
`grep -rn "Pattern" mur-core/src` should return only incidental/string matches.

---

## Key Insights

1. **Strangler Fig pattern:** We delete *users* of Pattern first, then the type last. This keeps the tree compiling at every commit.

2. **Shared modules:** Several `capture/` and `evolve/` files are shared with the Skill system and must be preserved:
   - `capture/emergence.rs`, `noise_filter.rs`, `starter.rs`, `feedback.rs`
   - `evolve/skill_evolve.rs`, `telemetry_reader.rs`, `cooccurrence.rs`, `feedback.rs`

3. **Fleet-sync gate:** Tier C cannot proceed until `server/`, `sync/`, and `federation/` move to skill equivalents. This is exactly what fleet-sync Phase 2 delivers.

4. **One bridge remains:** `cmd/skill_from_pattern.rs` bridges the gap, allowing one-way migration from legacy Pattern files to the new Skill subsystem. Kept intentionally until Phase C.

---

## Next Steps

1. **Tier A closure** — This document serves as the record of completion
2. **Coordinate with fleet-sync team** — Tier B work belongs in fleet-sync Phase 2 planning
3. **Schedule Tier C** — After fleet-sync Phase 2 ships
4. **Backfill documentation** — Update architecture docs to reflect Skill-only world

---

## References

- **Retirement plan:** `docs/superpowers/plans/2026-05-29-pattern-to-skill-retirement.md`
- **Fleet-sync Phase 2:** `docs/superpowers/plans/2026-05-29-fleet-sync-pro-phase1.md` (and successors)
- **Shipped PRs:** #313, #316, #317
- **Commits:** C2b dead code cleanup, C2b migration, A1-A4 retirement + docs
