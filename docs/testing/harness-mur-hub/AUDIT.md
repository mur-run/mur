# MUR Hub — Feature Audit (Phase 1)

Source of truth for coverage. Status as of 2026-06-04 on branch `test/harness-mur-hub`.

## Headline finding

The **current source is far more complete than the user thinks.** All 44 Tauri
commands have real implementations — **zero `todo!`/`unimplemented!`/stubs.**
The user's complaints trace to (a) running an **old installed build (Hub 0.1.0)**,
(b) **design gaps** (seed-skip, render-pending UX), and (c) **dev build lacks the
release-only MLX model**, not to unimplemented features.

## Environment reality (this mac)

| Thing | Value |
|-------|-------|
| Installed app | `/Applications/MUR Hub.app`, CFBundleShortVersionString **0.1.0** |
| mur CLI | `/opt/homebrew/bin/mur` **2.22.13** |
| `~/.mur/agents/` | **7 existing agents** (Author, …) → seed_if_empty SKIPS Mur |
| `~/.mur/agents/mur` | **absent** (never seeded) |
| `~/.mur/runtime/local_llm.url` | exists (written by installed 0.1.0 app) |
| `~/.mur/local_llm.conf` | **absent** (current code writes here → path drift vs installed) |
| `resources/models/default` | **empty (.gitkeep, 0B)** → no bundled MLX model in dev build |
| host_path | `/Applications/MUR Hub.app/.../mur-hub-gui` + `0.1.0` |

## Confirmed root causes of the 3 reported issues

1. **No default `Mur` agent** — `seed_mur::seed_if_empty` (lib.rs:331) only seeds when
   `~/.mur/agents/` is **entirely empty**. This user has 7 agents, so Mur is never seeded.
   FIX DIRECTION: seed when **no agent named `mur` exists** (by-name), not by-empty-dir;
   keep idempotent + non-clobbering. (Issue candidate.)
2. **STYLE "Not rendered yet"** — legitimate UI label for `render_status == pending`
   (DetailPanel.tsx:265). Any seeded/imported agent that never ran the wizard's 12-expression
   render stays `pending` forever, and the detail panel needs an obvious **Re-render CTA**
   that works outside the wizard. Render also needs an image-gen provider or MLX model;
   neither is present in a dev build. FIX DIRECTION: ensure Style tab exposes a working
   "Render"/"Re-render" action + graceful default-blob fallback messaging. (Issue candidate.)
3. **"Many functions not implemented"** — perception from the **old 0.1.0 build**. Current
   source implements all 44 commands. FIX DIRECTION: rebuild current branch, reinstall,
   re-verify each feature live. (Verify in TEST phase.)

## Implementation status — 44 commands (all IMPLEMENTED)

agents: list_agents, start_agent, stop_agent, open_dashboard, toggle_popover
wizard: wizard_open, _set_persona, _set_name, _set_preset, _set_behavior, _set_photo,
  _start_render (Gemini or Mock RenderJob), _finish (writes profile.yaml appearance), _cancel
first-launch: check_first_launch (lsregister on macOS), mark_first_launch_done
pet: pet_spawn_at, pet_close, pet_reposition, pet_return_to_hub, pet_list,
  pet_get_expression (webp→base64), hub_emit_event, pet_ack_bubble, pet_speak (Kokoro)
preset: import_preset_file, import_preset_url (https, 1MiB, 15s)
muragent: inspect_muragent_file, install_muragent_file, model_resolution_view, apply_agent_model
export: export_muragent_file
companion: companion_bridge_pending, _subscribe, _unsubscribe, companion_ack,
  companion_unread_count, companion_proactive, companion_quiet
cli: install_cli_tools (→/opt/homebrew/bin or ~/.local/bin)
brain badge: nudge_status, nudge_dismiss
detail: get_agent_detail, update_agent_detail

## Spec'd feature inventory (areas — see specs for full criteria)

Specs: 2026-05-11-mur-hub-companion-design.md, 2026-06-02-self-contained-hub-install-design.md,
2026-06-03-hub-detail-panel-plan-3.md, 2026-06-02-companion-media-skills-design.md.

1. **First-launch / default Mur** — seed built-in Mur concierge (local model), greets in zh-TW,
   guides user, offers upgrade. [ROOT CAUSE #1 here]
2. **Onboarding wizard (6 steps)** — Persona → Name+desc → Style preset (6 built-in) →
   Behavior (quiet/normal/lively) → [polaroid] photo → render 12 expressions w/ progress,
   per-image retry (3x), whole-batch → default-blob fallback.
3. **Agent detail panel (7 tabs)** — Persona(edit), Style(preset gallery + render status +
   Re-render), Behavior(radio), Skills(ro), MCP(ro), Permissions(ro), Inbox. [ROOT CAUSE #2]
4. **Tray/popover/dashboard** — single tray icon, popover (⌘⇧M, search ⌘F, categorized list,
   footer +New/⚙/📥, 300ms-hold drag), dashboard (sidebar+counts, grid/list/detail toggle,
   toolbar), Hub Settings window (ImageGen/Pet/DND/Voice/Presets/Updates).
5. **Pet windows** — transparent always-on-top, click/drag/right-click menu/double-click,
   bubble dwell + hover pause, hide-1h recall, voice indicator, drag-from-popover spawn.
6. **Companion** — inbox watcher (D5), proactive (behavior-gated), quiet hours, idle triggers.
7. **Preset import** — file + URL, YAML schema, 6 built-in presets, sha256 upgrade nudge.
8. **.muragent import/export** — file association (RunEvent::Opened), import→install+autostart,
   data-only export.
9. **CLI tools install** — tray menu item → symlink mur.
10. **Brain badge / nudge** — passive model badge, ceiling-triggered in-character upgrade nudge,
    durable dismissal (marker file), NOT timer/session-count based.
11. **MLX sidecar / concierge** — ephemeral-port OpenAI-compatible server, bundled Qwen3.5-2B
    (release-only), base URL to shared file, non-fatal if missing. [dev build lacks model]
12. **Media skills** — vlc-control (open/play/pause/seek/volume/status, YouTube), scene-explain
    (frame capture → local multimodal, zh-TW), idle auto-pause. (mur-core skills + MCP.)
13. **Cross-cutting** — expression state machine (priority+queue), 9 default triggers, 12-expr
    cache, Kokoro TTS + lip-sync, profile `appearance` section, hub config.yaml,
    migrate-to-hub, multi-monitor, runtime-bin resolution priority, release .dmg.

## Path-drift note (potential issue)

Installed 0.1.0 wrote `~/.mur/runtime/local_llm.url`; current code's
`mur_common::local_llm::write_base_url` targets `~/.mur/local_llm.conf`. Agents reading the
old path won't find the new one. Verify which path runtime/agents actually read. (Issue candidate.)
