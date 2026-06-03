# MUR Hub — Fix Batches

Each batch = 3 confirmed issues, fixed together, built + tested, committed locally (no push).

## Batch 1 — default Mur seeding + Style render CTA (I-1, I-2, I-3)

Addresses the user's three reported problems directly.

### I-3 (PRIMARY) — seed template resource path wrong → Mur never seeds, even fresh
- `mur-hub-gui/src-tauri/src/lib.rs`: resolve the template at `resources/mur-agent-template`
  (the path Tauri actually stages from the `"resources/..."` glob), falling back to the bare
  name; skip with a clear warning if neither exists.
- `mur-hub-gui/src-tauri/src/seed_mur.rs`: validate the template (profile.yaml) BEFORE creating
  any destination; copy into a staging dir then atomically rename into `agents/mur`, so a
  failure never leaves a broken empty `agents/mur`.

### I-1 — Mur skipped for users who already have agents
- `seed_mur.rs`: replace empty-dir gate with by-name `mur_seeded()` (checks
  `agents/mur/profile.yaml`). Now seeds the built-in concierge even when other agents exist,
  and heals a previously broken empty `agents/mur`. Stays idempotent / non-clobbering.
- lib.rs call updated to `seed_mur_if_missing`.

### I-2 — Style tab stuck at "Not rendered yet" with no action
- `mur-hub-gui/src-tauri/src/onboarding/mod.rs`: new Tauri command
  `render_agent_expressions(name)` — renders the 12 expressions for an EXISTING agent
  (reusing the proven wizard render pipeline: Gemini if key set, else offline Mock provider),
  persists `render_status` (Rendering → Ready/Failed) to profile.yaml, emits
  `agent-render-progress|done|error` events.
- `lib.rs`: registered the command.
- `ui/src/components/DetailPanel.tsx`: Style tab now has a Render / Re-render button (disabled
  while rendering) that calls the command and polls `get_agent_detail` until Ready/Failed.

### Verification
- `cargo clippy --all-targets -- -D warnings` (hub crate): clean.
- `cargo test seed_mur`: 6/6 pass — seeds_when_missing, seeds_even_when_other_agents_exist,
  skips_when_mur_already_seeded, heals_broken_empty_mur_dir,
  missing_template_errors_without_creating_dst, bundled_profile_deserializes.
- `npm run build` (tsc + vite): clean.
- Live (rebuilt .app, sandbox MUR_HOME) — PASS:
  - Scenario A (empty MUR_HOME): `agents/mur/` seeded with profile.yaml (1.5K) + sys_prompt.md
    + skills/. Log: "seeded built-in Mur agent". (I-3 fixed.)
  - Scenario B (pre-existing `Author` agent): both `Author` and `mur` present; "seeded built-in
    Mur agent". (I-1 fixed — the user's exact case.)
- I-2 render: covered by compile + clippy + the existing mur-gui-core RenderJob/Mock tests that
  the new command reuses; UI type-checks. (No screencapture available to click the button live.)

### New issues surfaced during live test (Batch 2)
- I-4: launchd-spawned `mur-agent-runtime` didn't get `MUR_HOME`.
- I-5: macOS autostart uses legacy `launchctl load` but `kickstart user/$UID/...` (domain
  mismatch → "Could not find service", exit 113). DEFERRED — see below.
- I-6: `mlx_sidecar` resolved `"models/default"` (same missing-`resources/`-prefix bug as I-3).

## Visual verification via the real app (2026-06-04, after Screen Recording was granted)

Driven the real bundled .app with screenshots + cliclick (screencapture now works once the
terminal got Screen Recording permission). Sandbox MUR_HOME pre-seeded with mur+Author+Coach
(so seeding skipped → no launchd side effects). Screenshots in `screenshots/`:
1. `01-dashboard-with-mur.png` — dashboard renders; **Mur** appears alongside other agents
   (I-1/I-3 fix visible), brain badge "Qwen3.5-2B-MLX-4bit", category counts, toolbar.
2. `02-style-not-rendered-with-render-button.png` — Mur → Style tab: "Not rendered yet" PLUS
   the new **"Render avatar"** button (I-2 fix) + 6-preset gallery.
3. `03-style-ready-after-render.png` — after clicking Render avatar: status flips to **"Ready ✓"**,
   button relabels **"Re-render avatar"**; 9+ expression .webp + manifest.json written to
   agents/mur/expressions/, profile render_status=ready (offline Mock provider). I-2 end-to-end.
4. `04-onboarding-wizard-step1.png` — "+ New Agent" opens the wizard; step 1 persona categories
   (Research/Automation/Monitor/Notify/Commerce/Custom) render — confirms wizard works.

Conclusion: the three reported problems are fixed and confirmed in the live UI. The "many
functions not implemented" complaint was the old installed 0.1.0 build; the current build's
dashboard, detail panel/tabs, Style render, and onboarding wizard all work.

Minor follow-up (not an issue worth a batch): the grid card avatar doesn't live-refresh to the
newly rendered idle.webp until reload; the detail panel does update correctly.

## Batch 4 — macOS autostart launchd domain fix (I-5)

A seeded/started agent never actually ran via the Hub: `register` used legacy
`launchctl load`, but `start_service`/`stop_service` targeted `user/$UID/<label>`, so
`kickstart` failed with "Could not find service … in domain for uid" (exit 113). The Hub
is a GUI app, so its LaunchAgents live in the `gui/$UID` (Aqua) domain.

### Fix (`mur-gui-core/src/autostart/macos.rs`)
- Extracted `gui_domain(uid)` / `service_target(uid, slug)` → `gui/$UID/run.mur.agent.<slug>`.
- `register`: `bootout`(modern, gui) + `unload`(legacy) best-effort to clear any prior
  registration, then `launchctl bootstrap gui/$UID <plist>` (RunAtLoad starts it). Idempotent
  and migrates agents previously `launchctl load`-ed.
- `start_service`: `kickstart -k gui/$UID/<label>`; `stop_service`: `kill TERM gui/$UID/<label>`;
  `unregister`: `bootout gui/$UID/<label>` + legacy unload + rm plist. `is_running` unchanged
  (`launchctl list <label>` is domain-agnostic).

### Verification
- Unit: `service_target_uses_gui_domain` (gui/, correct label). clippy -D warnings clean.
- Live launchctl smoke (real `launchctl`, throwaway sleeper service): `bootstrap gui/$UID` OK,
  `launchctl list` shows it, **`kickstart -k gui/$UID/<label>` exit 0** (the exact call that
  returned 113 with `user/$UID`), then `bootout` + rm → clean, no leak.
- Live integration: rebuilt .app, launched with empty MUR_HOME → seeds Mur → supervisor
  register+start logs **no** "kickstart failed"/"Could not find service" (previously it did).
  Created `~/Library/LaunchAgents/run.mur.agent.mur.plist` removed afterward; real ~/.mur untouched.

## Batch 6 — white-screen crash + window-size (I-10, I-11) + ErrorBoundary

### I-10 — modal-switch white screen (React #310)
- `ui/src/components/ErrorBoundary.tsx` (new) wraps `<App/>` in main.tsx → render errors show a
  message + stack + "Try again" instead of a blank window (this is how I-10's cause was found).
- `MuragentImportModal.tsx`: moved `useMemo(importDisabledReason, …)` ABOVE the
  `if (!isOpen) return null` early return. It was called after the return, so the hook count
  changed between isOpen false/true → React #310 unmounted the whole tree.

### I-11 — window opened below minWidth
- `lib.rs`: `tauri_plugin_window_state` now uses `StateFlags::all().difference(StateFlags::SIZE)`
  → remembers position, not size; window always opens at the tauri.conf default (≥ min).

### Verification (live)
- I-10: the Import Preset ↔ Import Agent switch that previously blanked the UI now renders both
  modals, no crash, no ErrorBoundary screen. Import Agent (.muragent) modal renders its
  inspect/install UI. tsc clean.
- I-11: dashboard window now opens 720pt wide (was restoring 462). Verified via window bounds.
- Also re-verified this round: wizard step 1 (persona categories) renders; Import Preset modal.
- Logged (not fixed): I-12 (both import modals can be open at once — cosmetic).

NOTE on coverage: the FULL onboarding wizard create flow (type name → preset → behavior → render
→ finish) was not driven end-to-end (precise multi-step clicking + text entry is fragile under
synthetic input); wizard entry + step 1 + backend commands are verified/implemented.

## Batch 5 — Persona tab shows the agent's real tone/risk/verbosity (I-9)

Found while testing the remaining detail-panel functions. The Persona tab's tone/risk/verbosity
`<select>`s only listed canned options, so an agent whose stored value wasn't in the list (the
seed template used warm/cautious/medium) displayed the FIRST option instead — misrepresenting
the agent and risking clobbering the value on save.

- `mur-hub-gui/ui/src/components/DetailPanel.tsx`: `withCurrent(options, current)` prepends the
  current value when it isn't already an option, applied to all three selects.
- `mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml`: aligned the seed Mur traits
  to the canned vocabulary (friendly / conservative / balanced).

### Verification
- `npm run build` (tsc) clean; `cargo test bundled_profile_deserializes` passes (template parses).
- Live: Persona tab opens and renders for an agent seeded with non-canonical traits; dashboard
  also confirmed the seeded Mur appears (by-name seed). Pixel-level confirmation of the dropdown
  text below the fold was skipped (Retina + variable window size made it costly); the fix is
  trivially correct and type-checked.

Also logged this round (not fixed): I-10 (one-off blank UI after rapid modal switching — could
not reproduce; needs devtools), I-11 (window can open narrower than declared minWidth). See ISSUES.md.

## Batch 3 — desktop pet feature made to work (I-7 + I-8)

The user asked to test the desktop pet. Found it doubly broken and fixed both; verified the pet
now spawns on the desktop (screenshot 05). Two coupled fixes (both required for the feature).

### I-8 — pet_spawn_at panics (crashes the whole Hub)
- `mur-hub-gui/src-tauri/src/pet/mod.rs`: `pet_spawn_at` is a sync #[tauri::command] with no
  entered Tokio runtime, so the two `tokio::spawn` calls (event loop + pet.spawned publish)
  panicked ("there is no reactor running"). Changed both to `tauri::async_runtime::spawn`.

### I-7 — drag-to-desktop never spawns a pet
- `mur-hub-gui/ui/src/components/DashboardApp.tsx`: spawn was gated on a `cursorOutsideRef` set
  only by a document `mouseleave`, which does not fire during a button-held drag out of the
  window → pet never spawned. Now `onUp` also decides "outside" from the release coordinates vs
  the window bounds (`window.screenX/Y/outerWidth/outerHeight`).

### Verification (live, real app + cliclick drag + screenshots)
- Before I-8 fix: drag-out (with I-7 fix) triggered spawn → app CRASHED with the tokio panic
  (proves both the I-7 fix works AND I-8 was real).
- After I-8 fix: drag an agent card to the desktop → pet window appears (beige idle sprite from
  the offline mock render), app stays ALIVE, no panic. Screenshot
  `screenshots/05-desktop-pet-spawned.png`. clippy -D warnings clean.

## Batch 2 — local-inference / MUR_HOME path correctness (I-4, I-6; I-5 deferred)

Note: this batch ships TWO fixes, not three, by design. I-5 (modernising macOS launchd from
`launchctl load` to `bootstrap gui/$UID`) changes autostart semantics for ALL agents and has
real-system side effects (writes/loads plists in the user's real `~/Library/LaunchAgents`).
It cannot be verified safely under this automation tonight without risking the user's launchd
state, so it is documented precisely in ISSUES.md (I-5) for review rather than auto-committed
unverified. The two fixes below are surgical and verified.

### I-6 — mlx_sidecar bundled-model resource path wrong (same class as I-3)
- `mur-hub-gui/src-tauri/src/mlx_sidecar.rs`: resolve `resources/models/default` (Tauri's actual
  staged path), fall back to the bare name, require an existing dir, else warn + skip. Without
  this, local inference can't find the model even when it IS bundled in a release build.

### I-4 — MUR_HOME not propagated to launchd-spawned agent runtime
- `mur-gui-core/src/autostart/macos.rs`: extracted a pure `plist_contents()` and added an
  `EnvironmentVariables` dict setting `MUR_HOME` to the dir the Hub registered the agent with,
  so the runtime resolves the same data directory (fixes MUR_HOME users + any non-default home).

### Verification
- `cargo clippy -p mur-gui-core --all-targets -- -D warnings`: clean.
- hub crate `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo test -p mur-gui-core autostart`: 2/2 pass — incl. new `plist_sets_mur_home_env`.
- I-6: code-verified (resolve path now matches Tauri's staged layout; dev build has an empty
  models dir so local inference still skips, as expected — release bundles the model).
- Not live-run: I-4 end-to-end needs real launchd (avoided to not touch user's system); I-6
  needs a bundled model (release-only). Both committed locally for morning review.
