# MuR Hub Companion — Design

**Date:** 2026-05-11
**Status:** Draft (brainstorming approved, pending user review before plan)
**Owner:** david
**Supersedes parts of:** `2026-04-29-mur-agent-gui-export-design.md` (per-agent .app surface)
**Related:** `2026-04-29-mur-companion-phase-1-1-design.md`, `2026-04-30-mur-agent-d2-onboarding.md`, `2026-05-02-mur-agent-d5-gui-bridge.md`, `2026-05-07-mur-agent-c6-idle-triggers.md`, D1 Voice (shipped)

## 1. Problem

A power user installs many mur agents. Each agent today ships as its own macOS `.app` bundle (mur-agent-gui), each with its own tray icon. The menu bar fills up; agents have no unified entry point; the "agent" feels like a background process, not a companion. There is also no visual identity per agent — persona is text-only.

We want one **MuR Hub** desktop app that:

1. Acts as the single entry point for all agents (menu bar + dashboard window).
2. Lets the user drag any agent out onto the desktop as a **companion pet** with expressions that react to events.
3. Generates per-agent expression art at onboarding using an LLM image model, with optional user-supplied source photos.
4. Replaces the per-agent `.app` model going forward.

## 2. Goals & Non-Goals

### Goals (v1)

- One cross-platform (macOS + Windows) Tauri 2 desktop app: **MuR Hub**.
- Two UI surfaces: menu-bar popover (daily entry) + dashboard window (power user).
- Per-agent `style_preset` (chibi / pixel / live2d / polaroid families) and `behavior_preset` (quiet / normal / lively).
- 6 themed built-in style presets including `chiikawa`, plus an internal `default-blob` fallback used on render failure; user-importable custom presets.
- Onboarding-time pre-render of 12 expressions per agent via configurable LLM image provider; local cache; fully offline at runtime.
- Drag agent from popover or dashboard onto the desktop → transparent always-on-top **pet window**.
- 9 default expression triggers driven by an event bus (companion inbox, idle scheduler, A2A status, OS focus, voice playback, user interactions).
- Speech-bubble UI + optional D1 Kokoro voice integration with DND/Focus and microphone-busy detection.
- Migration tooling (`mur agent migrate-to-hub`) and a phased deprecation of `mur-agent-gui`.

### Non-Goals (v1, deferred to v2)

- Shimeji-style pet that climbs window edges / crosses windows.
- Per-phoneme Live2D lip-sync (v1 ships open/close two-state mouth animation only).
- Pet-to-pet interaction animations.
- Cloud sync of agent state.
- iOS / Android companions.
- Runtime expression generation (every reaction generated on the fly).
- Linux GUI parity (CI continues to build non-GUI crates).

## 3. Architecture

### 3.1 Crate Topology

```
mur/  (workspace)
├── mur-common/              [existing] + StylePreset / BehaviorPreset / ExpressionTrigger types
├── mur-core/                [existing] + hub subcommands (hub doctor / hub import-preset / agent migrate-to-hub)
├── mur-agent-runtime/       [existing] + expression-event A2A emitter
├── mur-agent-gui/           [existing, legacy] maintenance mode; removed in M-h8
├── mur-gui-core/            [NEW lib] shared sidecar_manager / companion_bridge / a2a_client
└── mur-hub-gui/             [NEW Tauri 2 app] MuR Hub.app
    └── src-tauri/src/
        ├── hub/             popover + dashboard window
        ├── pet/             per-pet transparent always-on-top window
        ├── presets/         renderer registry + preset loader
        ├── expression/      state machine + trigger engine
        └── onboarding/      6-step wizard
```

Rationale: greenfield `mur-hub-gui` (Approach A) lets both apps ship in parallel; `mur-gui-core` prevents fork divergence of D5 / supervisor code. See `§9 Migration` for the deprecation plan.

### 3.2 Process Model & Signal Flow

```
                ┌──────────────────────────────────┐
                │  MuR Hub.app (mur-hub-gui)       │
                │  [Tray Popover]  [Dashboard]     │
                │           │             │        │
                │           ▼             ▼        │
                │     Pet Window Manager (Tauri)   │──► [Pet Window] × N  (transparent always-on-top)
                │           │                      │
                │     Sidecar Supervisor (gui-core)│
                └───────────┬──────────────────────┘
                            │ spawn + A2A
        ┌───────────────────┼─────────────────────┐
        ▼                   ▼                     ▼
  mur_agent_<a>       mur_agent_<b>          mur_agent_<…>
        │                   │                     │
        ▼                   ▼                     ▼
 ~/.mur/agents/<name>/{agent.yaml, expressions/*.webp, companion/inbox/, runtime.sock}
```

- **Hub spawns runtimes** as child processes (one per agent), supervises with exponential backoff restart (1s/2s/4s/8s/30s cap). Hub exit → SIGTERM all children, SIGKILL after 5s.
- **A2A v0.3 unix socket** per agent at `~/.mur/agents/<name>/runtime.sock`. Hub subscribes for `running / idle / error / message-arrived` events.
- **Companion inbox** continues to use D5 filesystem watcher pattern; the watcher lives in `mur-gui-core` and is consumed by Hub instead of mur-agent-gui.
- **Pet windows** are independent OS-level transparent windows; communication with Hub is in-process via `tauri::Channel`.

### 3.3 Why pet windows are separate OS windows

Pets must live at arbitrary desktop positions, above all other apps, with click-through outside the sprite:

- **macOS**: `NSPanel`, `level = .floating`, `isMovableByWindowBackground`, `backgroundColor = .clear`, `hasShadow = false`, `collectionBehavior = .canJoinAllSpaces`. Per-region mouse-event passthrough via `setIgnoresMouseEvents(true)`.
- **Windows**: `WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`. Per-pixel alpha via `SetLayeredWindowAttributes`; `WS_EX_TRANSPARENT` for full-window passthrough where applicable.

Tauri 2 `WebviewWindow` supports transparent + always-on-top on both platforms; the platform-specific hit-test and focus quirks are handled via `objc2` (macOS) and `windows` crate (Windows). v1 uses bounding-box hit-test; alpha-edge hit-test deferred to v2.

## 4. Data Model

### 4.1 `AgentProfile` Extension

`mur-common/src/agent.rs` gains a single nested field `appearance`:

```rust
pub struct AgentProfile {
    // ... existing fields
    pub appearance: AgentAppearance,
}

pub struct AgentAppearance {
    pub style_preset: String,               // e.g. "chiikawa", "vtuber-soft"
    pub behavior_preset: BehaviorPreset,    // Quiet | Normal | Lively
    pub source_image_path: Option<PathBuf>, // required for polaroid family
    pub expressions_dir: PathBuf,           // ~/.mur/agents/<name>/expressions/
    pub last_rendered_at: Option<DateTime<Utc>>,
    pub render_status: RenderStatus,
}

pub enum BehaviorPreset { Quiet, Normal, Lively }
pub enum RenderStatus {
    Pending,
    Rendering { done: u8, total: u8 },
    Ready,
    Failed { reason: String },
}
```

YAML: `~/.mur/agents/<name>/agent.yaml` gains an `appearance:` section. Agents predating v1 get default values (`style_preset: "default-blob"`, `behavior_preset: Normal`, `render_status: Pending`) via `mur agent migrate-to-hub`.

### 4.2 Style Preset Schema

Built-in presets ship in `mur-common/src/hub/builtin_presets/`. Imported presets live in `~/.mur/hub/presets/<id>.yaml`.

```yaml
id: chiikawa
display_name: ちいかわ
author: builtin
version: 1
family: chibi              # chibi | pixel | live2d | polaroid
description: "Nagano-style soft pastel chibi, vulnerable cute"

llm_image_gen:
  base_prompt: |
    Soft pastel chibi character in Nagano-style ちいかわ aesthetic.
    Tiny round body, oversized head, dot eyes, small 'w' mouth,
    pale cream / beige color palette, hand-drawn line, vulnerable cute.
  negative_prompt: "realistic, sharp lines, dark colors, adult"
  size: 512x512
  steps: 28

expressions:
  - { id: idle,       prompt_suffix: "sitting peacefully, soft smile" }
  - { id: smile,      prompt_suffix: "happy smile, slight blush" }
  - { id: wave,       prompt_suffix: "waving hello, one paw up" }
  - { id: think,      prompt_suffix: "thinking pose, finger on chin" }
  - { id: wow,        prompt_suffix: "surprised, eyes wide open" }
  - { id: sparkle,    prompt_suffix: "celebrating, sparkles around" }
  - { id: cry,        prompt_suffix: "teary eyes, holding back tears" }
  - { id: sleep,      prompt_suffix: "sleeping, Zzz above head" }
  - { id: peek,       prompt_suffix: "peeking from corner, shy" }
  - { id: talk_open,  prompt_suffix: "talking, mouth open" }
  - { id: talk_close, prompt_suffix: "talking, mouth slightly closed" }
  - { id: error,      prompt_suffix: "worried, hand on cheek" }

renderer:
  family: chibi
  default_size: { w: 96, h: 112 }
  idle_animation: breathe   # breathe | none
  blink_interval_s: [3.0, 6.0]
  crossfade_ms: 240
```

`polaroid` family adds `requires_source_image: true` and `img2img: true`.

Built-in themed presets (selectable in wizard step 3): `chiikawa`, `sanrio-pastel`, `sumikko`, `shimeji-retro`, `vtuber-soft`, `family-photo`. Plus the internal `default-blob` (SVG-only) — not offered in the wizard, used only as fallback when LLM render fails or before render completes.

**Expression list coverage:** of the 12 expression IDs, 9 are bound by default triggers (§4.4). The remaining three are reserved: `idle` is the always-on resting state, `talk_close` is auto-paired with `talk_open` by the voice indicator (200ms alternation, not separately triggered), and `wow` is available for user-defined trigger rules (e.g. "incoming Slack DM").

### 4.3 Behavior Presets (built-in, not user-extensible)

```rust
pub const BEHAVIOR_QUIET: BehaviorConfig = BehaviorConfig {
    idle_motion: IdleMotion::Sit,
    voice_default: false,
    bubble_dwell_s: 8,
    proactive_messages: false,
    expression_triggers_enabled: EXPRESSION_TRIGGERS_QUIET,
};

pub const BEHAVIOR_NORMAL: BehaviorConfig = BehaviorConfig {
    idle_motion: IdleMotion::Wander { interval_s: (180, 300) },
    voice_default: true,
    bubble_dwell_s: 6,
    proactive_messages: true,
    expression_triggers_enabled: EXPRESSION_TRIGGERS_NORMAL,
};

pub const BEHAVIOR_LIVELY: BehaviorConfig = BehaviorConfig {
    idle_motion: IdleMotion::Wander { interval_s: (60, 180) },
    voice_default: true,
    bubble_dwell_s: 5,
    proactive_messages: true,
    expression_triggers_enabled: EXPRESSION_TRIGGERS_LIVELY,
};
```

v1 `Lively` is essentially `Normal` with faster wander; the Shimeji-style edge-climbing is a v2 hook.

### 4.4 Expression Trigger Rules

Defaults in `mur-common/src/hub/triggers/default.yaml`; per-agent overrides at `~/.mur/agents/<name>/triggers.yaml`:

```yaml
triggers:
  - { on: "companion.message.new",   expression: wave,    dwell_s: 4, bubble: true }
  - { on: "idle.trigger.fired",      expression: peek,    dwell_s: 4, bubble: true }
  - { on: "agent.tool.running",      expression: think,   dwell_s: until_done }
  - { on: "agent.tool.completed.ok", expression: sparkle, dwell_s: 3 }
  - { on: "agent.error",             expression: error,   dwell_s: until_ack }
  - { on: "user.idle.30m",           expression: sleep,   dwell_s: until_active }
  - { on: "user.click.pet",          expression: smile,   dwell_s: 2 }
  - { on: "voice.playing",           expression: talk_open, dwell_s: lipsync }
  - { on: "os.focus.engaged",        expression: hidden,  dwell_s: until_focus_off }
```

### 4.5 Hub Global Config

`~/.mur/hub/config.yaml`:

```yaml
version: 1
image_gen:
  provider: "google-gemini"
  model: "gemini-2.5-flash-image"
  parallel_jobs: 3
pet_window:
  max_visible_pets: 5
  multi_monitor: true
dnd:
  hide_during_focus: true
  hide_during_fullscreen: true
voice:
  default_enabled: true
```

`image_gen.provider` resolves through the existing `mur model` registry; secrets reuse the existing `secret_ref:` mechanism. **No new credential storage is introduced.**

### 4.6 Expression Cache Layout

```
~/.mur/agents/<name>/
├── agent.yaml               # appearance section
├── triggers.yaml            # optional override
├── pet_position.json        # last desktop position per monitor
├── runtime.sock             # A2A unix socket
├── companion/inbox/         # D5 (unchanged)
└── expressions/
    ├── manifest.json        # { preset_id, rendered_at, sha256, expressions[] }
    ├── idle.webp ... error.webp
    └── _src.webp            # polaroid only
```

`manifest.sha256` records the preset YAML hash; a preset upgrade triggers a "re-render available" prompt.

## 5. Pipelines

### 5.1 Expression Render Pipeline

```
StylePreset → build_prompts() → 12 prompts
             → ImageGenProvider (Gemini default; OpenAI / fal / local ComfyUI configurable)
             → parallel_jobs=3 batches
             → post_process: webp(lossless q=90) + alpha-edge trim + resize
             → ~/.mur/agents/<name>/expressions/*.webp + manifest.json
             → emit AppearanceEvent::Ready
```

**Provider trait** (in `mur-gui-core`):

```rust
#[async_trait]
pub trait ImageGenProvider {
    async fn generate(
        &self,
        prompt: &str,
        negative: Option<&str>,
        size: ImageSize,
        source_image: Option<&Path>,
        cancel: CancellationToken,
    ) -> Result<DynamicImage, ImageGenError>;
}
```

**Failure handling:**

- Per-image failure → `missing` in manifest; trigger engine falls back to `idle`; dashboard shows "N expressions failed · Retry".
- Whole-batch failure → `RenderStatus::Failed { reason }`; `default-blob` SVG pack ensures the pet remains functional.
- In-flight cancel via `CancellationToken`; abort drops HTTP tasks and marks `Pending`.
- Progress streamed to UI via `tauri::Channel<RenderProgress>`.

### 5.2 Behavior Engine

```
Event sources                       ExpressionStateMachine               Pet Window
─────────────                       ──────────────────────               ──────────
A2A socket   ─┐
D5 watcher   ─┤                     ┌────────────────────┐               ┌──────────┐
IdleScheduler ┼─► EventBus ────────►│ resolve_trigger()  │── Expression ─►│ Renderer │
OS events    ─┤   (broadcast)       │ apply_rules()      │   Event       │ (Tauri)  │
UI clicks    ─┘                     │ debounce / merge   │               └──────────┘
                                    └────────────────────┘                     ▲
                                              │                                │
                                              ▼                                │
                                    expression_changed ───────────────────────┘
```

**State-machine rules:**

- Priority order (preempts lower): `error/cry > talk > think > one-shot reaction > idle`.
- Same-priority reactions queue; popped on `dwell_s` expiry.
- Special dwells: `until_done`, `until_ack`, `until_active`, `lipsync` — resolved by matching downstream events.
- 100ms debounce to prevent flicker when many events arrive at once.

**Event bus:** `tokio::sync::broadcast`. Every event carries `agent_id`; each pet's state machine and dashboard list filter by `agent_id`.

### 5.3 Speech Bubble + D1 Voice

The bubble is an in-pet-window React component above the sprite (not its own OS window). Max 280px wide, auto-wrap, hover pauses dwell.

Voice integration reuses D1's `kokoro_tts::synthesize(text, voice_id) -> AudioBuffer`:

1. Bubble appears → audio sent to `cpal` default output → `voice.playing` event fires.
2. State machine alternates `talk_open` / `talk_close` every 200ms; reverts on audio end.
3. **DND**: macOS `NSWorkspace.Focus`; Windows `SHQueryUserNotificationState`. Focus → voice disabled, bubble still shown.
4. **Mic busy**: macOS `AVCaptureDevice.isUsedByAnotherApplication`; Windows `IMMNotificationClient`. Mic busy → voice disabled (assume call in progress).

### 5.4 Drag-to-Desktop

1. `mousedown` on agent card → 300ms hold engages drag → translucent ghost sprite (`expressions/idle.webp`) tracks cursor.
2. `mouseup`:
   - Inside Hub UI → cancel; ghost animates back.
   - Outside Hub UI → Tauri command `pet::spawn_at(agent_id, screen_x, screen_y)` opens a transparent pet window; plays `wave` for 2s.
3. Context menu (right-click): `💬 Chat | ⚙ Settings | 👁 Hide 1h | 📥 Return to Hub | ❌ Close`.
4. Drag pet to reposition; saved to `pet_position.json` (per-monitor).
5. "Return to Hub" fades pet out with a recall animation to the matching dashboard card.

### 5.5 Cross-Platform Surface Differences

| Topic | macOS | Windows |
|-------|-------|---------|
| Transparent pet window | `NSPanel` + `level=.floating` + clear bg | `WS_EX_LAYERED` + per-pixel alpha |
| Cross-Space / virtual desktops | `collectionBehavior=.canJoinAllSpaces` | Same desktop only (no API) |
| Region click-through | `setIgnoresMouseEvents(true)` per-region | `WS_EX_TRANSPARENT` (whole window) |
| Focus / DND | `NSWorkspace.Focus` | `SHQueryUserNotificationState` |
| Always on top | `level=.floating` | `HWND_TOPMOST` |
| Tray icon | `NSStatusItem` (Tauri-wrapped) | `NOTIFYICONDATA` (Tauri-wrapped) |
| Hit-test boundary | Alpha threshold inside webview | Same |

v1 uses bounding-box hit-test on both platforms; alpha-edge hit-test deferred to v2.

## 6. UI Surfaces

### 6.1 Menu-Bar Popover (daily entry)

- Triggers: tray icon click, `⌘⇧M` global hotkey, dashboard minimize.
- Size: 280px wide × max 480px, internal scroll.
- Top: search input (`⌘F` focus), fuzzy match on `name`, `display_name`, `persona.description`.
- Middle: categorized list (PINNED / RESEARCH / AUTOMATION / MONITOR / NOTIFY / CUSTOM); each row shows avatar (idle expression thumbnail), name, status dot (green/grey/red), drag handle.
- Bottom: footer with `+ New agent`, `⚙ Hub Settings`, `📥 Import preset`.
- Drag: 300ms hold on row engages drag mode. Drop outside popover → `pet::spawn_at`.
- ESC closes; blur lose-focus also closes (NSPopover convention).

### 6.2 Dashboard Window (power user)

- Default 720×520; min 560×400.
- Sidebar (130px): `All / Pinned / Research / Automation / Monitor / Notify / Custom / + New Category` with live counts.
- Main area, three views (toolbar toggle):
  - **Grid** — 4×N cards (avatar, name, status, run/stop). Drag outside window spawns pet.
  - **List** — table with `model`, `last activity`, `#triggers fired today`.
  - **Detail** — selected agent opens a right-side slide-in panel with 6 tabs (`Persona | Style | Behavior | Skills | MCP | Permissions`), reusing the existing mur-agent-gui tab structure but switchable across agents in one window.
- Toolbar: `+ New Agent`, search (`⌘K`), view toggle, refresh, `⌘,` Hub settings.
- D5 companion inbox surfaces as an `Inbox` sub-tab in Detail; unread count appears on the sidebar agent row.

### 6.3 Pet Window

- Layers (back to front): sprite layer → bubble layer → voice indicator → hit-test mask → context menu.
- Interactions: left-click = `smile` 2s + `user.click.pet` event; left-drag = reposition + persist; right-click = context menu; double-click = open Hub popover focused on this agent; ESC on bubble = close bubble only.
- Bubble dwell counts down while not hovered; hover pauses, move-away resumes.
- "Hide 1h" removes the pet but keeps the supervisor signal flow (events still consumed, just not rendered); time-up triggers fade-in.

### 6.4 Onboarding Wizard (6 steps)

1. **Persona category** (existing enum).
2. **Name + description** (description seeds the D2 first memory).
3. **Style preset** — 6 built-in + `Import custom preset…`.
4. **Behavior preset** — 3 fixed (`quiet / normal / lively`); "Advanced" expands trigger YAML editor.
5. **(polaroid family only) Upload source photo** — privacy notice: local-only, img2img with optional original deletion.
6. **LLM render of 12 expressions** — grid progress, estimated cost (per provider), "Run in background" button.

Failure UX:

- Per-image: red `↻ retry`, up to 3 attempts.
- Whole-batch (no API key, quota): `🛟 Use default-blob, retry later` keeps the agent usable.

### 6.5 Hub Settings (separate window)

`⌘,` or dashboard toolbar. Sections:

- **Image Gen** — provider/model picker reading `mur model` registry, estimated per-onboarding cost.
- **Pet Defaults** — global max pets, auto-restore, multi-monitor behavior.
- **DND** — Focus / fullscreen behavior, quiet hours (default 22:00–07:00 forces quiet preset).
- **Voice** — global on/off, preferred voice id.
- **Presets** — manage imported presets, import from URL, export custom preset.
- **Updates** — Hub + mur CLI version check.

## 7. IPC Details

### 7.1 Sidecar Supervisor

`mur-gui-core::sidecar::Supervisor`:

- On Hub start: scan `~/.mur/agents/*/agent.yaml`; spawn `mur_agent_<name>` for each.
- Restart on crash: exponential backoff 1/2/4/8/30s cap.
- Child PID written to `~/.mur/agents/<name>/runtime.pid`; stale PID on next Hub start → kill then respawn (no orphans).
- Hub shutdown: SIGTERM all children; SIGKILL after 5s.

### 7.2 A2A Extension

Existing A2A v0.3 broadcasts `status` and `message`. Adds a new lightweight `appearance.progress` event so Hub can forward render progress to other UI surfaces (e.g. dashboard background tab) without going through the supervisor again. The runtime itself does not render; the event is for fan-out only.

## 8. Testing

### Unit
- Style preset YAML round-trip across all built-in presets.
- `ExpressionStateMachine` rule table tests: feed 100-event sequences, assert `current_expression` and queue.
- Supervisor restart timing: mocked child crash, verify backoff curve.
- Preset manifest sha256 invalidation on YAML change.

### Integration
- A2A socket smoke: spawn echo agent, verify Hub receives status/message.
- Expression cache write/read with `MockImageGenProvider`.
- `mur agent migrate-to-hub`: legacy v0 YAML in, v1 YAML out, `.bak` retained.

### Image-gen
- Default to `MockImageGenProvider` in CI (synthetic webp output).
- Gemini provider tested via `mockito` with fixtures.
- Real API only in weekly nightly job (capped < $1/week).

### UI (Tauri webdriver / Playwright)
- Popover open/close, search filter, drag-outside-popover invokes `pet::spawn_at`.
- Wizard happy path, polaroid branch, step-6 failure fallback.
- Dashboard detail panel switches agents in place (no window reload).

### Pet Window Snapshot
- Headless render of 12 expressions, bubble open/close, blink frame.
- Compared against `tests/snapshots/<expression>.png` on macOS and Windows CI.

### Cross-platform CI
- mac-13 / mac-14 / windows-2022; ubuntu-22.04 builds non-GUI crates only.
- mur-agent-gui drops out of CI in M-h8.

### Manual QA (per release)
- 20 simultaneous pets, idle CPU < 5%.
- Secondary-monitor unplug → fallback to primary.
- macOS Focus + Windows "Do not disturb" toggle.
- Mic-busy auto-disables voice during a call.
- D1 Kokoro lip-sync for Chinese and English sample lines.

## 9. Migration

**Phase 1 (M-h0 → M-h6, parallel)** — mur-agent-gui untouched; mur-hub-gui developed in parallel; CLI `mur agent export` unchanged.

**Phase 2 (M-h7, handoff)** — mur-hub-gui enters beta; Homebrew tap publishes `mur-hub`. mur-agent-gui marked `[legacy]` in its README. D5 companion inbox UI duplicated from mur-agent-gui into mur-hub-gui (sharing the bridge code via `mur-gui-core`). `mur agent migrate-to-hub` added: idempotent, writes `.bak`, fills `appearance` defaults, removes legacy `.app` autostart entries.

**Phase 3 (M-h8, deprecation)** — mur-hub-gui reaches v1.0. mur-agent-gui crate set `publish = false`; CI drops the crate. `build.sh` no longer bundles per-agent `.app`. `mur agent export` retains `.murcard.yaml` + install-manifest output so cards remain shareable.

Upgrade path: `brew upgrade mur && mur agent migrate-to-hub`.

## 10. Milestones

| ID | Title | Headline Deliverables |
|----|-------|----------------------|
| M-h0 | Workspace scaffold | New `mur-gui-core`, `mur-hub-gui` crates; workspace excludes; CI matrix gains mac+win. |
| M-h1 | Hub UI shell | Popover + dashboard window shells; scan `~/.mur/agents/*` and list; no drag, no pet yet. |
| M-h2 | Supervisor + A2A event bus | Extract sidecar_manager to mur-gui-core; spawn/supervise runtimes; broadcast status to UI. |
| M-h3 | Style preset system | `StylePreset` / `AgentAppearance` types; 6 built-in YAML; loader; `default-blob` fallback. |
| M-h4 | Image-gen pipeline + wizard | `ImageGenProvider` trait + Gemini + Mock; 6-step wizard; retry / batch-fail fallback; manifest. |
| M-h5 | Pet Window + drag | Transparent Tauri WebviewWindow; macOS NSPanel + Windows layered; drag-to-desktop; `pet_position.json`. |
| M-h6 | Expression engine + triggers | EventBus; ExpressionStateMachine; trigger YAML; bubble component. |
| M-h7 | Voice + DND + custom preset import | Kokoro TTS wired to bubble; mac Focus + Win DND; preset import UI; `mur agent migrate-to-hub`. |
| M-h8 | mur-agent-gui deprecation + Windows polish + v1 | D5 inbox moved into Hub; mur-agent-gui `publish=false`; Windows snapshot tests; release tap. |

Estimated effort 8–12 weeks single-engineer. M-h4 and M-h5 are the highest-risk and can be parallelized.

## 11. Risks & Mitigations

| Risk | Severity | Mitigation |
|------|----------|-----------|
| LLM output for chiikawa-style varies in quality | High | `quality_threshold` in preset; CLIP-score auto-retry; built-in hand-drawn `chiikawa` fallback pack. |
| Windows transparent + always-on-top jitter on high-DPI | Medium | v1 bounding-box hit-test only; alpha hit-test in v2; early multi-DPI testing. |
| Tauri 2 macOS webview flicker on transparent animations | Medium | Single canvas per pet; GPU layer hint; no DOM-layer animation. |
| Missing API key / quota → onboarding stalls | High | Step 6 fallback to `default-blob`; dashboard banner prompts API key configuration. |
| Polaroid family uploads user photos to cloud LLM | Medium | Provider configured with `no-store` headers; original-photo deletion default-on; explicit privacy notice in wizard. |
| Multi-monitor unplug → pet lost | Low | `pet_position.json` includes `display_id`; missing display → primary center fallback. |
| mur-agent-gui users hit migration confusion | Medium | `mur agent migrate-to-hub` idempotent + `.bak`; release-note banner. |

## 12. Open Questions (do not block v1)

- Shimeji-style edge-climbing for `Lively` (v2).
- Pet-to-pet interaction animations (v2).
- Community preset distribution: GitHub-repo model vs central index on app.mur.run.
- iOS widget / Apple Watch companion notifications.
- Cloud TTS providers beyond Kokoro (ElevenLabs, etc.).

## 13. References

- `2026-04-29-mur-agent-gui-export-design.md` — current per-agent `.app` and export model.
- `2026-04-29-mur-companion-phase-1-1-design.md` — relationship / situation enums reused here.
- `2026-04-30-mur-agent-d2-onboarding.md` — existing first-memory wizard that the new flow extends.
- `2026-05-02-mur-agent-d5-gui-bridge.md` — companion inbox bridge moved to `mur-gui-core`.
- `2026-05-07-mur-agent-c6-idle-triggers.md` — idle scheduler used as an event source.
- D1 Voice shipped — `kokoro_tts` reused for bubble narration.
