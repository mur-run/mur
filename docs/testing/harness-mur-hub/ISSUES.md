# MUR Hub — Issue Log

Statuses: CANDIDATE (suspected from code) · CONFIRMED (reproduced live) · FIXED (in a batch).
Batching rule: accumulate 3 CONFIRMED issues → one fix batch → commit (no push).

## Batch 1 — FIXED & verified (commit on test/harness-mur-hub)
- I-1, I-2, I-3 fixed; see FIXES.md. Live: empty + existing-agents both seed Mur.

## Open queue (Batch 2 accumulating — need 3 confirmed)

### I-4 — supervisor does not propagate MUR_HOME to spawned agent runtime — CONFIRMED (live)
- Seeded Mur's runtime logged `profile not found at /Users/david/.mur/agents/mur/profile.yaml`
  while Hub ran with MUR_HOME=/tmp/hub-harness/A. The child `mur-agent-runtime` defaults to
  ~/.mur because the supervisor spawn doesn't pass MUR_HOME through. Breaks MUR_HOME users
  (and all sandboxed runs). Fix: pass MUR_HOME env (and/or --mur-home) to the spawned runtime
  in mur-gui-core sidecar Supervisor. NOTE: production w/o MUR_HOME is unaffected.

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
