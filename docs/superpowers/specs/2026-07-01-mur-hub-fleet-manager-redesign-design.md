# MUR Hub — Fleet Manager Redesign (Mode × Run × Isolation)

**Date:** 2026-07-01
**Status:** Approved
**Supersedes non-goals from:** `2026-06-29-mur-hub-fleet-surface-design.md`

## Goal

The original Fleet Surface (`FleetCreateModal`/`FleetDetail`) only exposes plain-squad
fleets: name/goal/members/router, one-shot Run, Stop/Start, Export/Import. It explicitly
deferred loop/cron/budget config and parallel-mode selection to the CLI as v1 non-goals.

A fleet's actual shape is three orthogonal axes, all already implemented in
`mur-common`/`mur-core`/`mur-daemon` but invisible in the Hub:

1. **Mode** (`parallel:` block) — `Plain` (absent), `Speculative` (N tracks race the same
   goal, judge picks the best), `Partition` (split one file into disjoint regions, one
   track per region, merged back).
2. **Run** (orthogonal) — one-shot `run`, guarded `run --loop` (iteration/deadline/budget/
   stuck/`done_when` marker convergence), or daemon auto-run (`fleet_tick`, `interval:`/
   `cron:` trigger, gated on `MUR_FLEET_AUTORUN=1` + per-fleet budget + kill-switch).
3. **Isolation** (orthogonal switch) — `MUR_PARALLEL_EXEC=1` gives each track its own git
   worktree; off = members share the checkout.

This redesign brings all three into the Hub GUI.

## Non-goals

- Editing mode/tracks/judge/partition target after creation — recreate the fleet to
  change these (loop/budget/trigger ARE made editable; see below).
- Live partition-plan preview in the create form — `mur fleet partition-plan <name>`
  from the CLI covers preview.
- Per-iteration live progress for loop runs — still a single `fleet:run_done` at the
  end, same as today's one-shot run (matches the original spec's real-time-streaming
  non-goal).
- "Next predicted auto-run time" — only "last auto-run" (a plain `.last_run` sentinel
  read); computing next-fire for cron would duplicate scheduler math in the frontend.
- Visual cron builder or rubric-weight sliders — raw text / plain number inputs.
- Team-shared fleet auto-run policy — out of scope, unrelated to this redesign.

---

## Architecture

### 1. `mur-common` changes

`mur-common/src/config.rs` — add a `fleet.autorun` gate alongside the existing
`MUR_FLEET_AUTORUN` env var, following the exact pattern of the 10+ existing nested
`Config` sections:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetConfig {
    #[serde(default)]
    pub autorun: bool,
}

// on Config:
#[serde(default)]
pub fleet: FleetConfig,
```

No changes to `fleet.rs` or `parallel.rs` — `Fleet`, `FleetLoop`, `ParallelConfig`,
`TrackConfig`, `JudgeConfig`, `PartitionConfig` already support everything this redesign
surfaces.

### 2. `mur-core` changes

**New file `mur-core/src/cmd/fleet/settings.rs`** (mirrors `roster.rs`'s
load → mutate → save pattern exactly):

```rust
pub fn cmd_fleet_set_loop(mur_home: &Path, name: &str, loop_cfg: FleetLoop) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;
    fleet.loop_cfg = Some(loop_cfg);
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}
```

**`cmd/fleet/create.rs`** — `cmd_fleet_create` gains an optional `parallel:
Option<ParallelConfig>` param (currently hardcoded `None` at the Hub's call site). `mur
fleet create` gains no new CLI flags for this — setting up tracks/judge/partition has
always been a hand-edit-the-YAML affair for CLI users (there were never `create` flags
for it before this spec either), and that doesn't change here. Only the Hub gets a real
form for it; CLI parity for `parallel:` setup at creation is explicitly not part of the
"Yes, add CLI parity" decision below (that decision was scoped to loop/budget/isolation
only).

**`cmd/fleet/run.rs`** — isolation is currently a single-call-site env var check:

```rust
fn parallel_exec_enabled() -> bool {
    std::env::var(EXEC_FLAG_ENV).as_deref() == Ok("1")
}
```

This is unsafe to toggle per-click from a long-lived GUI process (mutating process env
races concurrent runs). Change to:

```rust
fn parallel_exec_enabled(force: bool) -> bool {
    force || std::env::var(EXEC_FLAG_ENV).as_deref() == Ok("1")
}
```

`cmd_fleet_run` gains a `force_worktree: bool` param, threaded to this call site.

**`cmd/fleet/loop_run.rs`** — **CONFIRMED during planning: no change here.**
`run_guarded`'s iteration loop calls the same `build_fleet_procedure` as `cmd_fleet_run`
but has none of `cmd_fleet_run`'s Tier-1 worktree machinery (`discover_repo_root`,
`worktree::create_tracks`, `inject_worktree_routing`, the `ParallelRun` bookkeeping) —
worktree isolation was never implemented for the guarded-loop path. Adding it is a real
design question (worktree lifecycle across N iterations: recreate each iteration and
lose uncommitted work, or reuse across iterations and redefine when they're torn down?)
that belongs in its own spec, not a parameter-threading footnote here. **Scope
correction:** the isolation checkbox in the Hub therefore only applies to the one-shot
`Run` button, not `Run as loop` — `cmd_fleet_run_loop`/`run_guarded` are unchanged,
`fleet_run_loop` is a pure new Tauri wrapper with no new mur-core work (the function
already takes exactly the override params the Hub needs). `--worktree` combined with
`--loop` on the CLI is rejected with a clear error rather than silently ignored.

**CLI parity** (`mur-core/src/cli/actions.rs`):
- New `mur fleet set-loop <name> [--trigger T] [--max-iterations N] [--deadline D]
  [--budget-usd U] [--done-when S]` — thin wrapper over `cmd_fleet_set_loop`.
- `mur fleet run <name> [job] --loop ... [--worktree]` — new `--worktree` flag, wired to
  the new `force_worktree` param (works for both one-shot and `--loop`).

### 3. `mur-daemon` changes

`mur-daemon/src/fleet_tick.rs` — the autorun gate currently checks only
`std::env::var("MUR_FLEET_AUTORUN")`. Extend to also accept the config flag, re-read
fresh each tick (same freshness as the existing env-var check, so flipping the Hub
toggle takes effect on the next 30s tick with no daemon restart):

```rust
fn autorun_enabled(mur_home: &Path) -> bool {
    autorun_flag(std::env::var("MUR_FLEET_AUTORUN").ok().as_deref())
        || mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).fleet.autorun
}
```

Either the env var or the config flag satisfies the gate (fail-closed if neither — no
change to that property). Per-fleet `budget_usd > 0` requirement, `.stopped` kill-switch,
and commander governance checks are unchanged.

### 4. Hub Tauri backend — `mur-hub-gui/src-tauri/src/fleet.rs`

| Command | Signature | Wraps |
|---|---|---|
| `fleet_create` | `(name, goal, members, router?, parallel?: ParallelPayload) → ()` | `create::cmd_fleet_create` (+parallel) |
| `fleet_set_loop` | `(name, trigger, max_iterations, budget_usd, deadline, done_when) → ()` | `settings::cmd_fleet_set_loop` — **NEW** |
| `fleet_run` | `(name, worktree: bool, app) → String` | `cmd_fleet_run(..., worktree)` |
| `fleet_run_loop` | `(name, max_iterations?, deadline?, budget_usd?, worktree: bool, app) → String` | `cmd_fleet_run_loop(..., worktree)` — **NEW**, same spawn_blocking + `fleet:run_done` pattern as `fleet_run` |
| `get_fleet_autorun` | `() → bool` | reads `Config.fleet.autorun` — **NEW** |
| `set_fleet_autorun` | `(bool) → ()` | writes `Config.fleet.autorun` — **NEW** |

`FleetDetail` (the Rust→TS serializable struct) gains two fields:

```rust
pub struct FleetDetail {
    // ...existing fields...
    pub loop_cfg: Option<FleetLoopView>,        // trigger, max_iterations, budget_usd, deadline, done_when, last_run
    pub parallel_summary: Option<ParallelSummary>,  // mode, track_count — drives the read-only Mode badge
}
```

`FleetSummary` (rail list) is unchanged — mode is shown only in the detail panel, keeping
each rail row a single line.

### 5. UI components — `mur-hub-gui/ui/src/components/fleet/`

**`FleetCreateModal.tsx`** — today's 4 fields (name/goal/members/router) unchanged. New
`Mode` radio group, default `Plain` (so existing users see nothing new unless they opt
in):

```
Mode
 (•) Plain        — broadcast the goal to every member
 ( ) Speculative  — N tracks race the same goal, judge picks the best
 ( ) Partition    — split one file into disjoint regions, one track per region

──── shown only if Speculative ────
 Judge model      [ <select, sourced from the same model list ModelCombobox uses> ]
 Tracks (min 2, pre-populated with 2 empty rows on selection)
   approach [...] model [(default) ▾]  ✕     [+ Add track]
 Pre-filters  ☐ cargo check   ☐ cargo clippy (deny warnings)

──── shown only if Partition ────
 Judge model      [ <select> ]   (also used by `fleet compare`/`cherry` to score tracks)
 Target file      [ repo-relative path ]
```

Client-side validation only: Speculative requires ≥2 tracks before submit; Partition
requires a non-empty target_file. Neither reaches the backend if invalid.

**`FleetDetail.tsx`** — two additions to the existing stacked-sections layout (Header →
Members → Send Job → Jobs → Danger Zone):

1. **Mode badge** in the header — read-only, e.g. `Speculative · 2 tracks`. Plain fleets
   show nothing (matches today exactly).
2. **Run control**, replacing the single `▶ Run` button:
   ```
   ☐ Use isolated worktrees (experimental)     ← only rendered if parallel_summary is Some;
                                                  applies to Run only, NOT Run as loop (the
                                                  guarded-loop path has no worktree support — §2)
   [ ▶ Run ]   [ Run as loop ▾ ]
   ```
   `Run as loop ▾` expands an inline row (iterations / deadline / budget, pre-filled
   from `loop_cfg` if set, editable for this run only — not saved) + `[ Go ]`. Both `Run`
   and `Go` disable the whole run area and show a spinner while in flight, same as
   today's single-Run pattern; re-enable on `fleet:run_done`.
3. **New "Settings" section** (between Members and Danger Zone):
   ```
   Trigger      [ Manual ▾ ]  (Manual | Interval | Cron)
                (Interval) Every [ 30m ]                  (humantime-ish: 30s/5m/2h/1d)
                (Cron)     [ */15 * * * * ]
   Max iterations [ 8 (default) ]
   Deadline       [ e.g. 2h, 30m, 1d ]                     (relative to loop start, NOT a calendar date)
   Budget (USD)   [ 0.00 ]   ⚠ shown if Trigger≠Manual and Budget≤0: "needed for auto-run"
   Done when      [ marker:DONE (optional) ]
   Last auto-run: 2026-06-30 14:02 UTC  (or "never")
   "Stopping this fleet (below) also blocks auto-run."
                                          [ Save Settings ]
   ```
   `Deadline` and the `Interval` value are parsed by `parse_duration`
   (`loop_run.rs:55`) — digits + optional `s`/`m`/`h`/`d` suffix (bare integer =
   seconds) only, **not** a calendar date. This matters because unparseable input
   doesn't error — `effective_deadline`/the interval check just silently treat it as
   absent (fail-OPEN: no deadline enforced / never fires), which is the wrong default
   for a safety-relevant guard. So unlike the Budget ⚠ (advisory-only), the Hub
   validates Deadline and the Interval value client-side with the same format
   (regex mirroring `parse_duration`) and **blocks Save** on a non-matching value —
   don't propagate the backend's silent-acceptance behavior into the GUI.
   `Cron` keeps no client-side validation beyond non-empty (5-field POSIX is already
   validated server-side via the existing `next_fire_after` parser; redoing cron syntax
   validation in TypeScript is unnecessary surface for this redesign).

**`GeneralSettings.tsx`** — one new row:
```
☐ Allow fleets to auto-run unattended
   Fleets with a trigger and budget configured will run on schedule without
   confirmation. Off by default. Requires per-fleet budget > 0.
```

### 6. Data flow

```
FleetCreateModal
  submit → fleet_create(name, goal, members, router, parallel?)

FleetDetail Settings
  on select        → fleet_detail already returns loop_cfg + parallel_summary
  Save Settings    → fleet_set_loop(...) → toast → refetch detail

FleetDetail Run area
  Run              → fleet_run(name, worktree) → spawn_blocking → fleet:run_done → refresh jobs
  Run as loop → Go → fleet_run_loop(name, overrides, worktree) → spawn_blocking → fleet:run_done → refresh jobs

Hub Settings → General
  on mount  → get_fleet_autorun() → setChecked
  on toggle → set_fleet_autorun(bool) → toast
```

No polling added — same "refresh on action or `fleet:run_done`" discipline as the
original spec.

---

## i18n keys (additive to `en.ts` / `zh-TW.ts`)

```
fleet.create.mode.plain / .speculative / .partition (+ one-line description each)
fleet.create.judgeModel / fleet.create.tracks / fleet.create.addTrack
fleet.create.preFilter.cargoCheck / .cargoClippy
fleet.create.targetFile
fleet.settings.title / .trigger / .triggerManual / .triggerInterval / .triggerCron
fleet.settings.maxIterations / .deadline / .budget / .budgetWarning / .doneWhen
fleet.settings.lastRun / .lastRunNever / .save / .saved
fleet.run.loop / .go / .worktree
settings.fleetAutorun.label / .description
```

## Error handling

Same pattern as the existing Fleet Surface throughout: all new `invoke()` calls wrapped
in `.catch → showToast(err, 4000)`. No new confirm-dialogs — Settings Save and the
autorun toggle are reversible, low-stakes edits (unlike Delete, which keeps its existing
`confirm()`).

Client-side validation (blocks submit/save, never reaches the backend):
- Create: Speculative requires ≥2 tracks; Partition requires a non-empty target_file.
- Settings: Deadline and the Interval trigger value must match `parse_duration`'s format
  (digits + optional `s`/`m`/`h`/`d`) — see §5 rationale (fail-open footgun, not just a
  cosmetic check). Budget≤0-with-non-manual-trigger stays an advisory warning, not a
  block (the backend already no-ops it safely, unlike the deadline/interval case).

## Testing

Computer-Use manual verification (this codebase has no existing frontend unit-test
harness for these components — confirm during planning):

1. Plain-fleet creation — confirm byte-identical to today's flow (regression).
2. Speculative creation (2 tracks, judge, pre-filters) → correct `parallel:` block on disk.
3. Partition creation (target_file) → correct `parallel:` block on disk.
4. Settings: set `trigger: interval:30m` + budget, Save → correct `loop:` block; budget
   warning appears/disappears correctly.
5. Run button → unchanged one-shot behavior (regression).
6. Run as loop → Go with overrides → `cmd_fleet_run_loop` invoked, `fleet:run_done` fires,
   jobs refresh.
7. Isolation checkbox + Run on a Speculative fleet → worktrees appear under `.worktrees/`.
8. Global "Allow auto-run" ON + fast test interval (e.g. `interval:1m`) + budget → next
   daemon tick actually fires the fleet.
9. Global toggle OFF (env var also unset) → same fleet does not auto-fire.

Rust unit tests (mirroring existing conventions):
- `cmd_fleet_set_loop`: create → set loop → load → assert fields (style of
  `roster.rs`'s `add_then_remove_member_syncs_fleet_and_is_idempotent`).
- `fleet_tick.rs` gate: extend the existing `autorun_flag` test style to cover the
  config-flag-only and both-set cases.
- `parallel_exec_enabled(force)`: true when forced regardless of env var state.

## Files changed

| File | Change |
|---|---|
| `mur-common/src/config.rs` | add `FleetConfig` + `Config.fleet` |
| `mur-core/src/cmd/fleet/settings.rs` | **New**: `cmd_fleet_set_loop` |
| `mur-core/src/cmd/fleet/mod.rs` | register `settings` module |
| `mur-core/src/cmd/fleet/create.rs` | none — `cmd_fleet_create` already takes `parallel: Option<ParallelConfig>` |
| `mur-core/src/cmd/fleet/run.rs` | `parallel_exec_enabled(force)`; `cmd_fleet_run` gains `force_worktree` |
| `mur-core/src/cmd/fleet/loop_run.rs` | none — confirmed no worktree logic exists here to gate (§2) |
| `mur-core/src/cli/actions.rs` | new `set-loop` subcommand; `--worktree` flag on `run` |
| `mur-daemon/src/fleet_tick.rs` | gate checks config.yaml OR env var |
| `mur-hub-gui/src-tauri/src/fleet.rs` | new/extended commands (§4) |
| `mur-hub-gui/src-tauri/src/lib.rs` | register new commands |
| `mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx` | Mode picker + conditional sections |
| `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` | Mode badge, Run/Run-as-loop split, isolation checkbox, Settings section |
| `mur-hub-gui/ui/src/components/settings/GeneralSettings.tsx` | autorun toggle row |
| `mur-hub-gui/ui/src/styles/components/fleet.css` | new `.fleet-create__mode-*`, `.fleet-settings__*`, `.fleet-run__*` classes (existing variables/spacing/naming conventions, no new design system) |
| `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts` | new keys (§ i18n) |
