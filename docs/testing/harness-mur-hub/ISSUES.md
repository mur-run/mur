# MUR Hub — Issue Log

Statuses: CANDIDATE (suspected from code) · CONFIRMED (reproduced live) · FIXED (in a batch).
Batching rule: accumulate 3 CONFIRMED issues → one fix batch → commit (no push).

## Batch 1 — FIXED & verified (commit on test/harness-mur-hub)
- I-1, I-2, I-3 fixed; see FIXES.md. Live: empty + existing-agents both seed Mur.

## Batch 2 — I-4 + I-6 FIXED (committed); I-5 DEFERRED for review

### I-4 — MUR_HOME not propagated to launchd-spawned runtime — FIXED (Batch 2)
- Was: seeded Mur's runtime logged `profile not found at ~/.mur/agents/mur/profile.yaml` while
  Hub ran with MUR_HOME=sandbox; launchd plist had no MUR_HOME env. Fix: plist now sets
  EnvironmentVariables/MUR_HOME (mur-gui-core/src/autostart/macos.rs, `plist_contents`).

### I-6 — mlx_sidecar model resource path missing `resources/` prefix — FIXED (Batch 2)
- Same bug class as I-3. Fix: resolve `resources/models/default` w/ fallback (mlx_sidecar.rs).

### I-5 — macOS autostart launchctl domain mismatch — FIXED (Batch 4) & verified live
- `register` uses legacy `launchctl load <plist>` (macos.rs:~66) but `start_service` uses
  `launchctl kickstart -k user/$UID/<label>` and `stop_service` similar → on modern macOS the
  service isn't found in that domain (`Could not find service "run.mur.agent.mur"`, kickstart
  exit 113). So a freshly-seeded/started agent never actually runs via the Hub.
- Proposed fix (NOT YET DONE — needs review + real-launchd verification): modernise to
  `launchctl bootstrap gui/$UID <plist>` (bootout first to be idempotent) in register,
  `kickstart -k gui/$UID/<label>` in start_service, `kill TERM gui/$UID/<label>` in stop,
  `bootout gui/$UID/<label>` in unregister. Blast radius: affects autostart for ALL agents;
  has real-system side effects (writes/loads in ~/Library/LaunchAgents). Verify by bootstrapping
  one test agent, confirming `launchctl print gui/$UID/run.mur.agent.<slug>`, then bootout+rm.
- WARNING for whoever runs this: launchctl operates in the USER domain regardless of MUR_HOME,
  so testing WILL touch ~/Library/LaunchAgents. During this session a stray
  `run.mur.agent.mur.plist` was created by sandbox launches and had to be removed
  (booted out + deleted). Isolate or clean up.

### I-8 — Desktop pet: pet_spawn_at PANICS (tokio::spawn in a sync command) — CONFIRMED (live) — FIXED (Batch 3)
- Spawning a pet crashed the whole Hub: `thread 'main' panicked at pet/mod.rs:114: there is no
  reactor running, must be called from the context of a Tokio 1.x runtime`. `pet_spawn_at` is a
  SYNC #[tauri::command] (no entered Tokio runtime) but called `tokio::spawn` (lines 114 + 206).
- This was MASKED by I-7 (the drag never reached pet_spawn_at); fixing I-7 surfaced the crash.
- Fix: `tokio::spawn` → `tauri::async_runtime::spawn` (holds the runtime handle; inner
  tokio::time/select then run on it). VERIFIED: pet now spawns, app stays alive, no panic.

### I-7 — Desktop pet: drag-to-spawn never fires (spawn gated on mouseleave) — CONFIRMED (live) — FIXED (Batch 3)
- DashboardApp.tsx grid-card drag: `onMouseDown`→300ms hold→`dragging=true` works (card enters
  dragging state, confirmed visually). On `mouseup` it only calls `pet_spawn_at` IF
  `cursorOutsideRef.current` is true, and that ref is set ONLY by a document `mouseleave`
  listener (lines 136/140/148-151).
- During a button-held drag OUT of the window, macOS captures mouse events to the origin window,
  so the document `mouseleave` does NOT fire → cursorOutsideRef stays false → no spawn. Confirmed
  under synthetic drag (cliclick): drag engages but pet never spawns; reproduced twice.
- Backend `pet_spawn_at` (pet/mod.rs:145) is correct (builds transparent always-on-top webview,
  position persistence, event loop, pet.spawned). So if pet doesn't appear, the bug is the UI
  spawn-gate, not the backend.
- NEEDS manual confirm (real mouse): drag an agent card from the dashboard out to the desktop —
  does a pet appear? If NO → real bug.
- Proposed robust fix: in `onUp`, decide "outside" by comparing release screenX/screenY to the
  Hub window bounds (or Tauri window position/size), instead of relying on `mouseleave`. Also
  apply to PopoverApp drag path if it shares the pattern.

## Remaining-functions test round (live UI)

Verified working: dashboard grid + sidebar counts, agent cards (Run/Stop/Share), detail panel
open/close + tab switching, Persona tab, Style tab (+Render), Behavior tab (Quiet/Normal/Lively),
Import Preset modal (From File / From URL), onboarding wizard step 1 (persona categories),
desktop pet (Batch 3). Brain badge shows model.

### I-9 — Persona tab shows WRONG tone/risk/verbosity for non-canned values — CONFIRMED (live) — FIXED (Batch 5)
- Agent traits not in the dropdown option lists (e.g. seed template had tone "warm", risk
  "cautious", verbosity "medium"; options are professional…/conservative…/concise…) made the
  <select> snap to the FIRST option → the panel showed professional/conservative/concise,
  misrepresenting the agent and risking clobbering on save.
- Fix: `withCurrent()` prepends the current value when it isn't a canned option (DetailPanel.tsx),
  so the real value displays for ANY agent; also aligned the seed template traits to canonical
  (friendly/conservative/balanced). Verified: Persona renders; template still deserializes.

### I-10 — One-off blank/white dashboard after a modal-close→modal-open sequence — SUSPECTED (not reproduced)
- After (MCP-tab misclick that closed the panel →) opening Import Preset → closing it → opening
  Import Agent, the whole React UI went blank (Rust app stayed alive; no panic — a frontend
  crash/unmount). Could NOT reproduce with a clean single "Import Agent" click. Needs the webview
  devtools console to capture the JS error. Candidate: an unmount/state error during rapid
  modal switching. Investigate with devtools before fixing.

### I-11 — Dashboard window can open narrower than its declared minWidth (560) — MINOR (observed)
- tauri.conf.json sets dashboard minWidth 560, but a launch showed the window at 462 pt wide;
  window size also varied between launches (923 vs 462). Cosmetic; low priority.

## Resolved
### I-1 — Default "Mur" never seeded for existing users — FIXED (Batch 1)
- `seed_mur::seed_if_empty` (seed_mur.rs:35) returns Ok(false) if ANY agent dir exists.
  This user has 7 agents → Mur is never created. Concierge/guide absent.
- Expected (install-design §7): a built-in Mur concierge present to greet/guide.
- Fix direction: seed when **no agent named `mur`** exists (by-name, idempotent, no clobber).
  Update lib.rs:331 call + seed_mur API + tests.

### I-2 — Style tab has no Render/Re-render action; no backend render-for-existing-agent — CONFIRMED (code)
- `StyleTab` (DetailPanel.tsx:235) renders status text only ("Not rendered yet" for pending,
  DetailPanel.tsx:265) with NO button to start/re-run a render. Spec (detail-panel-plan-3 §5.3)
  requires a "Re-render" button (Ready/Failed). Backend exposes wizard_start_render (WizardSession
  only) but NO command to render an EXISTING agent's expressions.
- Effect: any seeded/imported agent is stuck at "Not rendered yet" with no path forward → exact
  user complaint.
- Fix direction: add backend command (e.g. `render_agent_expressions(name)`) reusing the render
  pipeline, + a Render/Re-render button in StyleTab (pending→Render, failed/ready→Re-render),
  with graceful messaging when no image-gen provider/MLX model is available (default-blob).

### I-3 — Seed template resource path wrong → Mur fails to seed even on FRESH install — CONFIRMED (live) — PRIMARY
- Launched bundled .app with empty MUR_HOME. Log: `seed Mur failed: No such file or
  directory (os error 2)`. `agents/mur/` was created but EMPTY.
- Cause: `tauri.conf.json` declares `"resources/mur-agent-template/**/*"`, so the bundle
  lands the template at `Contents/Resources/resources/mur-agent-template`. But lib.rs:334
  resolves `"mur-agent-template"` via BaseDirectory::Resource → `Contents/Resources/
  mur-agent-template` (missing). copy_tree create_dir_all(dst) BEFORE read_dir(src) →
  leaves a broken empty `agents/mur`, which then defeats future seeding.
- Same prefix bug likely affects mlx_sidecar resolving `"models/default"`.
- Fix: resolve `resources/mur-agent-template` (try both, pick existing); don't create dst
  before validating src; seed atomically (staging dir + rename); heal broken empty dir.

## Test environment note
- `screencapture` unavailable in this automation context ("could not create image from
  display"). Driving = launch real .app + verify via tracing log + sandbox filesystem +
  backend direct tests. App process runs fine; no blank-screen crash in logs.

## Confirmed-live

## Notes / lower-priority candidates

- local_llm path drift: installed app wrote `~/.mur/runtime/local_llm.url`; current code writes
  `~/.mur/local_llm.conf`. Verify which path agents actually read; if mismatched → I-? .
- DetailPanel opens to "inbox" tab by default (line 27/31) rather than Persona/Style; minor UX.
