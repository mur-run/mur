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

### New issue surfaced during live test (queued for Batch 2)
- I-4: supervisor spawns `mur-agent-runtime` WITHOUT propagating `MUR_HOME` → seeded agent's
  runtime looked for the profile at real `~/.mur/agents/mur/profile.yaml` instead of the
  sandbox. Production (no MUR_HOME) is consistent, but MUR_HOME users are broken. See ISSUES.md.
