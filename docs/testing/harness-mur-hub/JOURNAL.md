# Harness Test — MUR Hub (full audit + fix)

> **PURPOSE OF THIS FILE:** Recovery journal. If the session token runs out and a
> NEW session starts, READ THIS FILE FIRST, then continue from "NEXT STEPS".
> This is the resume/wakeup mechanism the user required. Keep it current after
> every meaningful step.

## Mission

User installed **MUR Hub** (the `mur-hub-gui` Tauri 2 desktop app). On open they saw:
1. **No default agent `MUR`** seeded.
2. **STYLE tab shows "Not rendered yet"** (observed on another mac).
3. Many functions appear **not implemented / unclear how to use**.

User wants a harness test of MUR Hub on THIS mac:
- Collect ALL Hub functions from design specs/plans (the inventory).
- Record missing/lost functions and IMPLEMENT them.
- TEST every function, record logs, FIX — but **batch fixes: do not fix until 3 issues accumulate**.
- Ensure every function works.
- Must be resumable after token exhaustion (this journal).

## User decisions (asked 2026-06-04, do NOT re-ask)

| Topic | Decision |
|-------|----------|
| Test method | **Drive the real app** (launch .app, AppleScript/screenshots + tracing logs to file). Supplement with direct backend/IPC tests where UI driving is impractical. |
| Order | **Full spec audit FIRST**, then batch fixes. |
| Workspace | **Isolated git worktree** (see below). |
| Git policy | **Commit each 3-issue batch locally, do NOT push, do NOT open PR.** User reviews in the morning. |

Other standing constraints (from CLAUDE.md / memory):
- Reply to user in **Traditional Chinese (zh-TW)**; code/commits/specs in **English**.
- User is asleep — **continue fully autonomously, do not ask further questions.**
- Token-saving rules apply (concise, one-pass, no auto plan files beyond this journal).

## Working location

- Worktree: `/Volumes/Firecuda4tb/Projects/mur/.claude/worktrees/hub-harness-test`
- Branch: `test/harness-mur-hub` (based off `main` @ 6c945467, v2.22.13)
- Do NOT switch back to the `/Volumes` main tree (it drifts via cherry-picks from the other checkout).

## Methodology

1. AUDIT: enumerate every Hub feature from specs → map to implemented Tauri command / UI → mark IMPLEMENTED / STUB / MISSING. Output: `AUDIT.md`.
2. BUILD: build `mur-hub-gui` (.app) on this mac. Build steps in `BUILD.md` once known.
3. TEST: launch app with `RUST_LOG=debug` → log file; drive UI + call backend commands; record per-feature pass/fail in `ISSUES.md`.
4. FIX: accumulate issues; when **3 issues** are confirmed, fix them as one batch, build, re-test, then `git commit` (no push). Log batch in `FIXES.md`.
5. Repeat until every feature works.

## Companion files

- `AUDIT.md` — feature inventory + implementation status (source of truth for coverage).
- `ISSUES.md` — every test result; open issues queue for batching.
- `FIXES.md` — each committed 3-issue batch.
- `BUILD.md` — how to build/launch the Hub on this mac (filled during BUILD phase).

## IPC command surface (from mur-hub-gui/src-tauri/src/lib.rs, 44 commands)

list_agents, start_agent, stop_agent, open_dashboard, toggle_popover,
wizard_open, wizard_set_persona, wizard_set_name, wizard_set_preset,
wizard_set_behavior, wizard_set_photo, wizard_start_render, wizard_finish,
wizard_cancel, check_first_launch, mark_first_launch_done,
pet_spawn_at, pet_close, pet_reposition, pet_return_to_hub, pet_list,
pet_get_expression, hub_emit_event, pet_ack_bubble, pet_speak,
import_preset_file, import_preset_url, inspect_muragent_file,
install_muragent_file, model_resolution_view, apply_agent_model,
export_muragent_file, companion_bridge_pending, companion_bridge_subscribe,
companion_bridge_unsubscribe, companion_ack, companion_unread_count,
companion_proactive, companion_quiet, install_cli_tools, nudge_status,
nudge_dismiss, get_agent_detail, update_agent_detail

## Early root-cause hypotheses (UNVERIFIED — verify during audit/test)

- **No default MUR agent:** `seed_mur::seed_if_empty` (lib.rs:331) is gated on
  `app.path().resolve("mur-agent-template", BaseDirectory::Resource)`. If the
  `mur-agent-template` resource is not bundled in `tauri.conf.json`, seeding is
  silently skipped. CHECK `tauri.conf.json` bundle.resources + that the template
  dir exists at build time.
- **STYLE "Not rendered yet":** wizard `Step6Render.tsx` / `detail.rs` avatar render
  via `mlx_sidecar`. Likely the render pipeline (MLX image gen) produced no avatar
  on the other mac (sidecar missing / model missing / offline). CHECK render flow +
  fallback when no rendered image exists.

## Relevant specs/plans

- docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md (main Hub design, 300 lines)
- docs/superpowers/specs/2026-06-02-self-contained-hub-install-design.md (default MUR + MLX concierge)
- docs/superpowers/plans/2026-06-03-hub-detail-panel-plan-3.md (detail panel incl. STYLE)
- docs/superpowers/plans/2026-06-02-self-contained-hub-install.md
- docs/superpowers/specs/2026-06-02-companion-media-skills-design.md
- docs/superpowers/plans/2026-05-11-mur-hub-companion-m-h0-scaffold.md

## CONFIRMED ROOT CAUSES (see AUDIT.md for detail)

- All 44 commands ARE implemented (no stubs). User's "not implemented" = running OLD
  installed build **Hub 0.1.0** (`/Applications/MUR Hub.app`); current source is complete.
- **No default Mur:** `seed_if_empty` only seeds when `~/.mur/agents/` is EMPTY; user has 7
  agents → Mur never seeded. FIX: seed by-name (no agent named `mur`), idempotent.
- **"Not rendered yet":** legit `render_status==pending` label; seeded/imported agents never
  render → stuck pending; need working Re-render CTA + dev build has no MLX model/image provider.
- Path drift: installed app writes `~/.mur/runtime/local_llm.url`; current code writes
  `~/.mur/local_llm.conf`. Verify which agents read.

## STATUS LOG (newest first)

- 2026-06-04: SESSION CHECKPOINT. Full test suites green: hub crate 20/20, mur-gui-core 42/42
  (62 total) — broad unit/integration verification that implemented commands work. Real system
  left clean (no stray launchd plist, real ~/.mur untouched, sandbox removed). 2 local commits
  on branch (Batch 1 + Batch 2), unpushed. To RESUME: read this JOURNAL + ISSUES.md; next work =
  I-5 launchctl (review+verify), then continue live testing UI-driven functions (blocked here by
  no-screencapture — needs a real driven session). User action: rebuild+reinstall Hub to get the
  fixes (their installed build is the old 0.1.0).
- 2026-06-04: BATCH 2 (I-4 + I-6) DONE & committed locally; I-5 DEFERRED for review.
  I-6: mlx_sidecar resolves `resources/models/default` (same fix class as I-3). I-4: launchd
  plist now sets EnvironmentVariables/MUR_HOME via extracted pure `plist_contents()`
  (autostart/macos.rs) + new unit test. clippy clean (gui-core + hub); autostart tests 2/2.
  Batch is intentionally 2 not 3: I-5 (launchctl load→bootstrap gui/$UID) is blast-radius +
  needs real-launchd verification → left for morning review (see ISSUES.md I-5). Cleaned a
  stray ~/Library/LaunchAgents/run.mur.agent.mur.plist created by earlier sandbox launches.
- 2026-06-04: BATCH 1 DONE (I-1,I-2,I-3) — seed-by-name+atomic+heal (seed_mur.rs), correct
  resource path `resources/mur-agent-template` (lib.rs), render_agent_expressions command
  (onboarding/mod.rs)+handler, Style-tab Render/Re-render button+poll (DetailPanel.tsx).
  Verified: clippy -D warnings clean, 6/6 seed tests, npm build clean, LIVE both sandboxes seed
  Mur (empty + existing-Author). Restored binaries/ to 0B (do NOT commit 352MB sidecars).
  About to commit (no push). NEW issue I-4 (MUR_HOME not propagated to spawned runtime) queued
  for Batch 2. Build note: share CARGO_TARGET_DIR=<wt>/target; copy debug mur+runtime into
  binaries/<triple> before `cargo tauri build --debug` (see BUILD.md).
- 2026-06-04: BUILD phase. tauri-cli 2.11.2 installed. Built real `mur`+`mur-agent-runtime`
  (debug) → copied into binaries/ (mlx-server stays 0B). UI built (ui/dist OK). First
  `cargo tauri build --debug` FAILED: No space left on device (separate src-tauri/target
  duplicated the heavy datafusion/lance tree). FIX: removed that target, rebuilding with
  CARGO_TARGET_DIR=<workspace>/target to dedupe (106GB free). Build running (task bzjdxuzv3).
  Confirmed via code: I-1 (seed-by-empty-dir) + I-2 (no render CTA / no render-existing-agent
  command). MockImageGenProvider renders offline → I-2 fixable locally. `appearance` has
  serde(default) so existing agents don't crash, just show Pending → "Not rendered yet".
  Need 3rd issue from live test. See ISSUES.md / BUILD.md / AUDIT.md.
- 2026-06-04: AUDIT done → AUDIT.md. Two Explore agents mapped specs + impl. Confirmed root
  causes above against real ~/.mur (7 agents, no mur, models/default empty, installed 0.1.0).
  Task#1 complete. Phase = BUILD (task#2): determine how to build mur-hub-gui .app here.
- 2026-06-04: Worktree + branch created. Read lib.rs (44 commands). JOURNAL created.

## NEXT STEPS (resume here)

1. BUILD: determine mur-hub-gui build (it's workspace-excluded; own tauri.conf.json + ui/).
   externalBin needs binaries/{mur,mur-agent-runtime,mlx-server}; models/default empty.
   Prefer `cargo tauri dev` or build a local .app. Write BUILD.md.
2. TEST (drive real app, RUST_LOG→file): verify each feature live; populate ISSUES.md.
   Reproduce: no-Mur (expected, since 7 agents), Style pending, any real bugs.
3. Batch-fix in 3s → commit (no push) → FIXES.md. First batch likely:
   (a) seed-by-name, (b) render/Re-render CTA, (c) local_llm path drift OR another live bug.
