# MUR Pack S1 — De-Shadow (root-cause fix for vendored builtin skills) — Design

**Status:** Design / spec (S1 of the Pack governance program)
**Date:** 2026-07-22
**Builds on:** the Pack governance north-star (`2026-07-22-mur-pack-governance-design.md`, §3.2 never-shadow, §8 S1) and S2 (`2026-07-22-mur-pack-s2-shadow-cleanup.md`, shipped #741 — promoted `mur-native-tools` to builtin + added the `mur skill doctor` `shadow-drift` check). S2's rollout attempt surfaced a gap this spec closes.

## 1. Goal

Kill the actual drift root-cause the governance program targets: agent-local **vendored copies of builtin skills** that shadow the shipped global builtin via `load_all` (agent-local wins on name collision) and never receive updates. Deliver a working de-pin path and an automatic, safe cleanup — **CLI-only**. Defer the unified pack manifest/kind/adapter kernel to when the capability kind (S3) needs it (YAGNI).

## 2. Context — what S2's rollout uncovered

- An agent's skills live in **two** profile structures plus on-disk dirs: `profile.skills: Vec<String>` (references — the load list) and `profile.installed_skills: Vec<SkillCardEntry>` (denormalized cards). Actual skill **injection** comes from the on-disk `~/.mur/agents/<agent>/skills/<name>/skill.yaml` dirs (via `load_all`, which shadows the global store on name collision). The `installed_skills` cards feed only the agent **card** (A2A/display) and CLI **autocomplete**.
- The live concierge (`~/.mur/agents/mur`) carries **4 true shadows** — `mur-compress`, `parallel-code`, `video-analyze`, `watch-together` — as `installed_skills` cards + on-disk dirs, NOT as `skills:` refs (its `skills:` list is only `concierge` + `brainstorming`). Plus `mur-native-tools`, which becomes a shadow once S2's new builtin is synced.
- **`mur agent skill remove <agent> <name>` cannot remove them.** Its `resolve_skill_id` searches only `profile.skills` (refs) in three forms (full id / basename / stem); it never looks at `installed_skills` cards or the on-disk dir. So every de-pin attempt returned "not found", and S2's `shadow-drift` remediation string (which points at that command) is wrong for this case.
- **Source of the vendored copies is legacy**, not current code. Current `mur init` writes builtins to the global store only (`ensure_mur_skill`). The current Hub seed (`mur-hub-gui` `seed_missing_bundled_skills`) copies only its bundled template `skills/` — which contains exactly `concierge` + `brainstorming` (verified), neither a builtin — so it cannot introduce a builtin shadow and will not re-vendor the 4 after de-pin. The concierge's shadows came from an older seed/add path.

## 3. Decisions

| Question | Decision |
|---|---|
| De-pin mechanism | Extend the existing `mur agent skill remove` into the **complete** de-pin verb: resolve a name across `skills:` refs / `installed_skills:` cards / on-disk dir, and remove **every** form present. |
| Remediation string | Fix S2's `shadow-drift` finding remediation to the working command form once `remove` resolves cards/dirs. |
| Batch cleanup | Add a `ShadowDepinRepair` to the existing `mur skill doctor --fix` (M5b `Repair`) framework: auto-remove ONLY shadows byte-identical to the global builtin; report diverged ones, never auto-remove. |
| Go-forward guard (Hub) | **Deferred** to an optional defensive Hub follow-up. The current Hub bundle is clean (concierge+brainstorming only), so no builtin shadow can be (re)introduced today. Not in S1's CLI scope. |
| Kernel (manifest/kind/adapter) | **Deferred** to S3 when the capability kind needs it. |

## 4. Design

### 4.1 `mur agent skill remove` — complete de-pin (`mur-core/src/cmd/agent/skill.rs`)
Today `cmd_skill_remove(name, query)` resolves only against `profile.skills` and returns "not found" otherwise. New behavior: resolve the target name against **all three** locations and remove each that is present, in one call:
1. **`skills:` ref** — if `resolve_skill_id` matches, remove it from `profile.skills` (existing behavior).
2. **`installed_skills:` card** — if any `SkillCardEntry.name == <basename>`, drop it via `profile.installed_skills.retain(|c| c.name != basename)`.
3. **on-disk dir** — `~/.mur/agents/<name>/skills/<basename>/` — remove the directory (existing "delete backing subdir if orphaned" logic generalizes: delete when no remaining `skills:` ref points at it).

The command succeeds if the skill was present in **any** location (not just refs); it errors only when the name is found in none. `<basename>` derivation matches `resolve_skill_id`'s existing forms (full id `skills/foo`, basename `foo`, stem). Save the profile atomically (existing `save_profile`). Changes apply on the agent's next restart (unchanged semantics; note in output).

### 4.2 Fix the `shadow-drift` remediation (`mur-core/src/cmd/skill_doctor.rs`)
Change the finding's remediation from `mur agent skill remove {agent} skills/{name}` to `mur agent skill remove {agent} {name}` (basename form, which §4.1 now resolves across cards/dirs). Message unchanged.

### 4.3 `ShadowDepinRepair` for `mur skill doctor --fix` (`mur-core/src/skill_repair/`)
The doctor already runs a `Vec<Box<dyn Repair>>` under `--fix` (M5b). Add one impl:
- `check_id() -> "shadow-drift"` (matches the S2 finding's `check_id`, so it is dispatched for those findings).
- `apply`: re-enumerate agent-local shadows exactly as `run_shadow_drift` does, and for each whose `content_hash_for_origin` **equals** the global builtin's (the `Severity::Ok` / identical class), perform the §4.1 removal (card + dir + any ref). Diverged shadows (`Severity::Warn`) are **left untouched** — reported only — so any genuine local edit is never destroyed.
- Register it in the `repairs` vec in `cmd_doctor`.
- Mark the identical-shadow findings `fixable: true` and the diverged ones `fixable: false` in `run_shadow_drift`, so `--fix` acts on exactly the safe set and the UI labels them correctly.

## 5. Migration / rollout
After this ships and a new `mur` is installed + `mur sync` writes `mur-native-tools` to the global store:
`mur skill doctor --fix` on the concierge removes the 4 identical shadows (`mur-compress`, `parallel-code`, `video-analyze`, `watch-together`) and `mur-native-tools`; any diverged one is reported for manual `mur agent skill remove mur <name>`. Restart the concierge (Hub-managed) to apply. `concierge` (identity) and `brainstorming` (registry, not a global builtin) are never flagged or removed.

## 6. Out of scope / deferred
- **Hub go-forward guard** (skip builtin-named templates in `seed_missing_bundled_skills`) — optional defensive follow-up; not needed while the bundle is concierge+brainstorming only.
- Unified pack **manifest / kind / adapter kernel** — S3+.
- Reworking the two-structure model (`skills` refs vs `installed_skills` cards) into one — larger refactor; not required to fix drift.

## 7. Testing
- **§4.1 remove**: (a) a name present only as an `installed_skills` card + on-disk dir (no `skills:` ref) is fully removed (card gone, dir gone) and the command succeeds; (b) a name present only as a `skills:` ref still removes as before; (c) a name present in none errors; (d) a name present in all three removes all three.
- **§4.2**: the `shadow-drift` finding's remediation string is the basename form and, fed to `remove`, actually resolves (integration-level assertion on the string).
- **§4.3 repair**: with an identical-content shadow → `--fix` removes it (card+dir) and a re-scan reports no finding for it; with a diverged shadow → `--fix` leaves it in place and it is still reported (`Warn`, not auto-removed); an agent-only skill (no global twin) is never touched.
