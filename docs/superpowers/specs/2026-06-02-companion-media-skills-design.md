# Companion Media Skills — Watch-Together for MuR Agents (`vlc-control` + `scene-explain`)

**Status:** Design
**Date:** 2026-06-02
**Scope:** macOS (Apple Silicon) first; skills themselves are cross-platform where VLC is.
**Companion spec:** `2026-06-02-self-contained-hub-install-design.md` (owns the
onboarding/UX framing of the "watch together" scene; this spec owns the skill
architecture).

## 1. Problem / Goal

The flagship post-install "must try" scene is Mur offering to **watch a movie with the
user and explain what is on screen**, warmly and in Traditional Chinese, **fully local
and private**. Realizing it requires two reusable MuR skills, bundled with Hub and
available to any MuR agent:

- **`vlc-control`** — detect VLC, open a local file **or a YouTube URL**, and control
  playback (play / pause / seek / volume / query position).
- **`scene-explain`** — capture the currently displayed frame and explain it using a
  local multimodal model, in the user's language.

Design constraint: explanation must run on the **local bundled multimodal model**
(default `Qwen3.5-2B`, upgrade `Qwen3-VL`). No frame is uploaded to any cloud — this
is the differentiator versus cloud screen-analysis tools.

## 2. Research Basis (2026-06)

- **Video understanding:** local VLMs are viable. The bundled `Qwen3.5-2B` is natively
  multimodal (handles text + images); `Qwen3-VL` (256K context, frame-by-frame,
  hours-long video) is the upgrade for long-form. Adaptive frame sampling
  (VideoBrain-style) is a future optimization, not required for v1 (the user drives
  pausing).
- **VLC control:** `vlc-mcp-server` (piebro) demonstrates controlling VLC via its
  **HTTP interface** — robust, cross-platform, the right fit for agent control.
  `python-vlc` / libVLC bindings expose `video_take_snapshot`, the cleanest way to
  grab the current frame. We adopt the HTTP interface for control and libVLC snapshot
  (or VLC's snapshot command) for frame capture.

Sources: roboflow local VLM guide, BentoML open-source VLM guide, DataCamp top VLMs
2026, `github.com/piebro/vlc-mcp-server`.

## 3. Architecture

Both skills follow the existing MuR skill model: a YAML manifest teaches the agent
*when / why* to use the capability (cf. `mur-core/src/skills/mur_project_search.yaml`),
while the actual actions are exposed as **tools via the MuR MCP server**
(`mur-mcp-server`) so the agent can invoke them mid-conversation.

```
Agent (Mur)
  │  decides to watch / explain  (taught by skill manifests)
  ▼
mur-mcp-server tools
  ├── vlc.open(path|youtube_url) / vlc.play / vlc.pause / vlc.seek / vlc.status   ──► VLC HTTP interface
  └── scene.explain(prompt?)                                                       ──► frame snapshot ──► local MLX VLM
```

- **Control transport:** VLC launched/ensured with its HTTP interface enabled
  (`--extraintf=http --http-host=127.0.0.1 --http-port=<ephemeral> --http-password=<random>`).
  Port and password are generated at runtime and stored in the agent runtime config —
  **no hardcoded port or password** (project rule).
- **Frame capture:** `scene.explain` triggers a snapshot of the current frame (libVLC
  `video_take_snapshot`, or VLC's snapshot, written to a temp path), then feeds the
  image to the local multimodal endpoint from the install spec (§6 there).
- **Model:** reuse the bundled local MLX server. Default `Qwen3.5-2B` (multimodal);
  if the user has upgraded to `Qwen3-VL`, `scene-explain` uses it automatically.

## 4. `vlc-control` Skill

Capabilities:

- **Detect** VLC (macOS: `/Applications/VLC.app`; configurable path; cross-platform
  lookup elsewhere).
- **Open** a source:
  - local media file path;
  - **YouTube URL** — VLC resolves YouTube via its bundled resolver; for resilience
    against YouTube changes, optionally shell out to `yt-dlp` to resolve the stream
    URL (bundling/availability of `yt-dlp` is an implementation decision, see §8).
- **Control:** play, pause, toggle, seek (absolute/relative), volume, and **status**
  (position, duration, title, playing/paused).
- All control via the VLC HTTP interface; no DRM-protected services (see §7).

The manifest teaches Mur to use these when the user wants to watch/control video and
to prefer status-before-action so narration aligns with the current frame.

## 5. `scene-explain` Skill

Capabilities:

- **Capture** the current frame from the active VLC instance.
- **Explain** via the local multimodal model: describe what is on screen, who/what is
  visible, and — when the user asks — interpret a line, a scene, or context. Output in
  the user's language (default zh-TW, warm tone).
- **Optional text context for accuracy:** when a subtitle track (local) or YouTube
  captions are available, pass the nearby caption text alongside the frame to ground
  the explanation (cheap, improves "他剛說的是什麼意思？"). Optional enhancement, not
  required for v1.

Privacy: frames and captions are processed locally; nothing leaves the machine.

## 6. Scene Orchestration Hooks

The warm proactive offer ("我看到你有裝 VLC！…") and idle auto-pause are owned by the
companion/onboarding layer (install spec §17), but rely on this spec's primitives:

- **Detection trigger:** a VLC-installed check that the companion can poll/observe on
  first run to decide whether to make the offer.
- **Idle auto-pause:** reuse the existing C6 idle-trigger infrastructure to call
  `vlc.pause` when the user is away, and resume on return.

## 7. Out of Scope (YAGNI)

- DRM-protected streaming services (Netflix, Disney+, etc.) — frames cannot be
  captured; not supported. Mur should decline gracefully.
- Continuous/automatic real-time narration of every frame (cost, intrusiveness). v1 is
  **user-driven**: explain on pause/ask. Adaptive auto-narration is a future option.
- Editing, downloading, or redistributing video content.
- Windows/Linux-specific VLC paths beyond best-effort detection (macOS first).

## 8. Open Implementation Questions (resolve during planning)

- **YouTube resolution:** rely on VLC's built-in resolver vs. bundle/depend on
  `yt-dlp` for resilience — and licensing/size implications of bundling `yt-dlp`.
- **Frame capture path:** libVLC `video_take_snapshot` (requires a libVLC binding or a
  small native helper) vs. VLC HTTP/RC snapshot command. Pick by reliability and
  whether VLC is embedded or external.
- **MCP tool surface:** exact tool names/params for `vlc.*` and `scene.explain` and how
  they register in `mur-mcp-server`.
- **Subtitle/caption sourcing** for the optional text-context enhancement.

## 9. Testing

- **Unit:** VLC detection on macOS; runtime port/password generation (no hardcoded
  values); manifest validity.
- **Integration:** `vlc.open` a local file and a YouTube URL → status reflects
  playback; `scene.explain` captures a frame and returns a non-empty local-model
  explanation; idle trigger calls `vlc.pause`.
- **Manual:** end-to-end "watch together" — Mur offers, plays a YouTube clip, user
  pauses and asks "這一幕在演什麼？", Mur explains in zh-TW, offline; DRM service is
  declined gracefully.

## 10. Affected Components

- `mur-core/src/skills/` — new `vlc-control` and `scene-explain` skill manifests
  (bundled with Hub).
- `mur-mcp-server` — `vlc.*` and `scene.explain` tools.
- Local multimodal endpoint — reused from the install spec (§6 there).
- Companion/onboarding layer — detection + proactive offer + idle auto-pause wiring
  (framing owned by the install spec §17).
