# Self-Contained MuR Hub — Double-Click-to-Life End-User Install (macOS)

**Status:** Design
**Date:** 2026-06-02
**Author:** brainstorming session
**Scope:** macOS (Apple Silicon) first. Windows/Linux deferred.

## 1. Problem

Today an end user (a non-developer friend) cannot realistically install and run a
MuR agent:

- `mur-agent-runtime` has no first-class distribution. `resolve_runtime_target()`
  (`mur-core/src/cmd/agent/mod.rs:120`) looks for the runtime binary next to the
  `mur` binary or on `PATH`; the crates.io publish of the runtime is "allowed to
  fail" in `release.yml`. A user who installed only `mur` ends up with an agent
  symlink pointing at a missing target.
- `mur-hub-gui` has **no** end-user install path at all. Its `README.md` documents
  only a developer dev-loop (`npm run dev` + `cargo tauri dev` in two terminals).
  `release.yml` does not build, sign, or ship the Hub app.

The result: trying MuR requires Rust, npm, and a manual binary dance. There is no
"must try" moment.

## 2. Goal

On macOS (Apple Silicon), a non-developer can:

1. Download one signed, notarized `.dmg`.
2. Drag **MuR Hub** to Applications.
3. Open it — a built-in concierge agent named **Mur** is already alive and speaks
   immediately, offline, with **no API key and no signup**, in fluent Chinese
   (Traditional preferred) and other languages.
4. Receive a `.muragent` file from a friend, double-click it, and watch that agent
   install and come alive in Hub.

No CLI, no npm, no Rust at any point. The CLI is available to power users as an
opt-in, not a prerequisite.

This realizes the existing strategy pillar "give-to-a-friend = Host signed once +
data-only `.muragent`" (see `2026-05-11-mur-hub-companion-design.md` and the export
UX pillar): the **Host** is the self-contained Hub app.

## 3. Chosen Approach

**Approach C** — a single self-contained, signed `MuR Hub.app` that embeds `mur` +
`mur-agent-runtime` + a bundled local model + the seed "Mur" agent, **plus** an
"Install command-line tools…" menu item for power users.

Rejected alternatives:

- **B (CLI installer + thin Hub shell):** keeps two installs, two notarizations, and
  PATH coupling — exactly the pain we are removing.
- **A (self-contained, no CLI menu):** good, but leaves power users with no clean way
  to get the CLI. C is A plus a small, well-understood menu item (the VS Code
  "shell command" pattern).

## 4. Distribution Architecture

`MuR Hub.app` is the single shipped artifact, delivered inside a `.dmg`.

The Tauri bundle (`mur-hub-gui/src-tauri/tauri.conf.json`) already targets
`["app", "dmg"]`, already registers the `.muragent` file association, and already
uses identifier `run.mur.hub`. We extend it to embed binaries, the inference
backend, and the model, mirroring the existing pattern in
`mur-agent-gui/src-tauri/tauri.conf.json` (`externalBin` + `resources`).

Bundle contents:

- `externalBin`: `mur`, `mur-agent-runtime` (placed under `binaries/`).
- `resources`:
  - the MLX inference sidecar (see §6),
  - the bundled model weights (see §5),
  - the seed "Mur" agent template (profile + system prompt + concierge skill, §7).

Estimated `.dmg` size: ~1.5–1.8 GB (dominated by the model). Acceptable for a 2026
desktop download.

## 5. Bundled Model (Local Brain)

Requirement: a **small, multilingual, strong-Chinese (Traditional preferred)
instruct model** that runs snappily on Apple Silicon with **zero key and no
network**.

Research (2026-06) findings that drive the choice:

- Qwen3.5 small series (released 2026-03-01): 0.8B / 2B / 4B / 9B, natively
  multilingual (~201 languages), MLX-optimized. The Qwen family is the strongest
  open family for Chinese.
- Qwen3.5 uses a Gated DeltaNet hybrid attention architecture that is **poorly
  supported on llama.cpp (~14× slower)** but well-optimized on **MLX**. This rules
  out a GGUF/llama.cpp backend and confirms MLX — consistent with the repo's
  existing MLX-first preference in `mur-core/src/cmd/init_local.rs`.

Selection:

| Role | Model | Rationale |
|------|-------|-----------|
| **Default bundled brain** | `Qwen3.5-2B-MLX-4bit` | Best balance of wow / size / Chinese. 30–50 tok/s on Apple Silicon, runs offline, ~1.3 GB at 4-bit. |
| Optional upgrade (wizard download) | `Qwen3.5-4B-MLX-4bit` | More capable when the user wants it; ~2.5 GB. |
| Rejected | 0.8B | Too weak to carry a convincing concierge conversation. |

The default model id is **configuration, not a hardcoded constant** (per project
rule "no hardcoded values"). It lives in config so it can be swapped without code
changes; `Qwen3.5-2B-MLX-4bit` is only the default value.

Model weights are **not committed to git**. They are fetched during the release
build (CI step / cache / LFS-external) and embedded into the bundle at package time.

## 6. Inference Backend (MLX Sidecar)

The Hub spawns a local inference server that exposes an OpenAI-compatible HTTP
endpoint on `127.0.0.1:<port>`, pointed at the bundled model.

- **Backend:** MLX (Apple Silicon). GGUF/llama.cpp is explicitly rejected for the
  Qwen3.5 architecture (§5).
- **Packaging decision (implementation):** prefer a **native MLX-Swift sidecar
  binary** so the `.app` does not have to embed a full Python runtime. Fallback if
  the native server is not ready in time: a frozen `mlx-lm` server (e.g.
  PyInstaller). This is an implementation choice to be finalized in the plan; the
  spec requires only "an MLX-backed, OpenAI-compatible local server bundled as a
  sidecar, no Python install required by the user."
- **Port:** chosen at runtime (ephemeral / configurable), written into the agent's
  runtime config. **No hardcoded port.**
- **Lifecycle:** started lazily on first need and tied to the Hub process; stopped
  when Hub exits.
- **Provider wiring:** reuse the existing provider plumbing. The repo already speaks
  an Ollama-style, key-less local endpoint (`mur-core/src/extract_llm.rs`,
  `init_local.rs`). The seed Mur agent's model points at the local server using the
  same mechanism. A `local`/`bundled` provider variant is added if needed; it must
  require no API key.

## 7. Seed "Mur" Concierge Agent

On first launch, if no agents exist in `~/.mur/agents/`, Hub seeds a built-in agent
named **Mur** from the bundled template:

- Copies the template into `~/.mur/agents/mur/` (profile.yaml, sys_prompt.md, a
  concierge skill).
- Creates the per-agent symlink and starts the agent via the existing lifecycle path
  (`mur-core/src/cmd/agent/lifecycle.rs`).
- Mur's model defaults to the local MLX server (§6).
- Mur's system prompt frames it as the product's guide: greet the user, explain what
  MuR does, and walk them through creating their own first agent or connecting a
  larger model.

Seeding is **idempotent**: it runs only when no agents exist (or when the seed agent
is absent), so it never clobbers a user who already has agents or who deleted Mur on
purpose.

"Mur" as the concierge name is intentional — it doubles as the product mascot and
keeps branding consistent. (Minor downside: same name as the product; acceptable for
a default guide persona.)

## 8. Runtime Path Resolution

`resolve_runtime_target()` (`mur-core/src/cmd/agent/mod.rs:120`) gains a new
**first-priority** branch: when the running executable is inside a macOS `.app`
bundle (`…/Contents/…`), resolve `mur-agent-runtime` from the bundle's
`Resources`/`MacOS` directory.

Order becomes:

1. `MUR_AGENT_RUNTIME_BIN` env override (unchanged).
2. **App-bundle-relative location (new).**
3. Next-to-current-exe (unchanged).
4. Bare filename on `PATH` (unchanged).

This makes the embedded runtime authoritative inside Hub, so the runtime no longer
needs independent distribution. CLI-only installs keep working via the existing
branches.

## 9. First-Run and `.muragent` Open Flow

**First run:**

1. Welcome screen.
2. Optional microphone / voice permission prompt (companion voice already exists).
3. Local MLX server starts; seed Mur agent appears and greets the user.

**`.muragent` double-click:**

1. The OS routes the file to Hub via the registered association (already configured).
2. The Tauri shell handles the file-open event (both cold-start and
   already-running cases).
3. Hub invokes the existing agent import path (`mur agent` import / `--load`) to
   install the package into `~/.mur/agents/`.
4. The new agent is started and brought to the foreground — it comes alive in Hub.

Import must reuse existing, tested import logic rather than re-implementing it.

## 10. Command-Line Tools Menu (Approach C)

A menu item, "Install `mur` command-line tools…", symlinks the bundled `mur` into a
PATH directory (`/opt/homebrew/bin`, falling back to `~/.local/bin`), prompting for
elevation only when required. End users can ignore it; power users get the CLI in one
click. The CLI is thereby an opt-in, not a prerequisite.

## 11. Release Pipeline

Add a macOS job to `release.yml`:

1. Build the Hub UI (`mur-hub-gui/ui`: `npm ci && npm run build`).
2. Fetch the bundled model weights (cache/external; not from git).
3. `cargo tauri build` for `mur-hub-gui`, embedding `mur`, `mur-agent-runtime`, the
   MLX sidecar, and the model.
4. `codesign` → `notarytool submit` → `stapler staple` (reuse the existing signing
   identity and notarization flow already used for `mur-*.dmg`).
5. Upload `MuR-Hub-aarch64-apple-darwin.dmg` to the GitHub Release.

The main README Quick Start gains a "Download MuR Hub for macOS" entry pointing at
this artifact.

## 12. Out of Scope (YAGNI)

- Windows and Linux Hub installers.
- Single-file signed `.muragent` (signing complexity; deferred per export pillar).
- Hosted free-trial inference credits (contradicts local-first; adds backend/cost).
- Committing model weights to git.
- Bundling the larger 4B model by default (offered as an opt-in download instead).

## 13. Testing

- **Unit:** the new app-bundle branch in `resolve_runtime_target()`; local provider
  configuration produces a key-less endpoint config.
- **Integration:** Mur seeding is idempotent (no-op when agents exist); `.muragent`
  open → import → start path installs and launches an agent.
- **Manual (clean Mac):** download → drag → open → Mur greets in Traditional
  Chinese; Gatekeeper/notarization passes (`spctl`); `.muragent` double-click brings
  an agent to life; "Install command-line tools" makes `mur` available on PATH.

## 14. Affected Components

- `mur-hub-gui/src-tauri/tauri.conf.json` — externalBin, resources, dmg.
- `mur-hub-gui/src-tauri/` — first-run flow, file-open handler, MLX sidecar
  supervision, CLI-tools menu, Mur seeding trigger.
- `mur-core/src/cmd/agent/mod.rs` — `resolve_runtime_target()` app-bundle branch.
- `mur-core/src/cmd/init_local.rs` / provider plumbing — key-less local MLX provider.
- Seed "Mur" agent template assets (new bundled resource).
- `.github/workflows/release.yml` — Hub build/sign/notarize/upload job.
- `README.md` — macOS Hub download in Quick Start.

## 15. Open Implementation Questions (resolve during planning)

- Native MLX-Swift sidecar vs frozen `mlx-lm` — pick based on readiness; spec
  requires only a Python-free user experience.
- Exact model-hosting mechanism in CI (HF download + cache vs release asset mirror).
- Whether the local provider is a new provider variant or a reuse of the existing
  Ollama-style provider pointed at the sidecar port.

## 16. Model-Upgrade Nudge

The bundled `Qwen3.5-2B` is deliberately small. Mur should help the user discover
that connecting a stronger brain (their API key, or a local 4B/larger model) makes
her more capable — **without nagging**, which would destroy the warm, friendly
feeling the whole onboarding is built around.

Design — "capability-ceiling-triggered, in character, once, remembered":

- **Passive, always-available affordance:** a low-key "brain" badge in the UI shows
  the current model and offers an upgrade entry point. Always present, never
  interrupts. This is the discovery path for users who go looking.
- **Active prompt only when the local model hits a real ceiling:** when the bundled
  model genuinely struggles with a task the user actually asked for (long reasoning,
  code, complex scene explanation) or the user voices a wish the small model can't
  meet, Mur says — once, in character — something like: "這個我現在的小腦袋有點吃
  力～你願意幫我接上更聰明的大腦嗎？" with a one-tap link to the model wizard.
- **Dismiss-and-remember:** dismissing the prompt is durable; Mur does not re-ask.

Explicitly **not** allowed: timer-based or session-count-based upsell prompts. This
follows the existing "companion nudge = emergence-only, not spam" principle.

## 17. Signature First-Use Scene: Watch Together (VLC / YouTube)

The flagship "must try" scene beyond first launch: if VLC is installed, Mur offers to
**watch a movie with the user and explain what's on screen**, in Traditional Chinese,
warmly — and entirely locally/privately, because the bundled model is natively
multimodal (a cloud tool would have to upload your screen; MuR does not).

High-level flow (warmth + WOW):

1. **Detect** VLC (`/Applications/VLC.app` on macOS).
2. **Warm proactive offer** (optionally spoken via the existing Kokoro TTS): "我看到
   你有裝 VLC！想一起看部電影嗎？我可以陪你看，也能幫你解說畫面、人物，或聽不懂
   的橋段～"
3. **Pick / consent:** user picks a local file **or a YouTube URL**, or points Mur at
   what's already playing; Mur drives playback.
4. **WOW — live narration:** the user can pause anytime and ask "這一幕在演什麼？";
   Mur grabs the current frame, runs it through the local multimodal model, and
   explains, offline.
5. **Companionship:** emotional reactions; auto-pause when the user steps away (reuses
   the existing C6 idle triggers).

This scene reuses existing companion presence, voice (Kokoro TTS / whisper STT), and
idle-trigger infrastructure. The new capabilities — controlling VLC and explaining
frames (including YouTube playback) — are specified separately in
**`2026-06-02-companion-media-skills-design.md`** as the `vlc-control` and
`scene-explain` skills. This install spec owns only the onboarding/UX framing of the
scene; that spec owns the skill architecture. The default bundled model choice in §5
(`Qwen3.5-2B`, natively multimodal) is what makes this scene possible locally.
