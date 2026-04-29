# murmur — `mur agent export --format gui` Design

**Status:** Spec v1 — 2026-04-29
**Authors:** David + Claude.
**Depends on:** P0a (`2026-04-22-murmur-p0-agent-runtime-design.md`), P0a.5 (fleet `2026-04-23-murmur-fleet-architecture-design.md`), P0a.6 (rekey `2026-04-24-murmur-agent-rekey-design.md`).
**Out of scope (separate spec cycles):** P2 signed `.app` notarization automation · P2 SBOM emission · P2 auto-update · P2 fully reproducible builds · P2 community theme marketplace · P2 GUI-driven multi-agent management.

---

## 0. Executive Summary

`mur agent export` today produces two artifacts: a `.murpkg` archive (`pkg`, for sharing with other mur users) and a self-contained CLI binary (`bin`, for recipients who do not have `mur` installed). Both ship the agent's profile, identity, skills, and prompt as a single executable surface.

This spec adds a third format: **`gui`** — a click-to-launch desktop app with a menubar/tray icon, a full settings window that replaces the `mur agent <subcommand>` CLI surface for the bundled agent, and a swappable colour-scheme + icon theme system. The output is a `MyAgent.app` bundle on macOS, an `.AppImage` on Linux, or an `.exe` on Windows; each is self-contained (no `mur` CLI dependency on the recipient's machine), 50–80 MB, and stores all per-install state under `~/.mur/agents/<name>/` so the agent remains interoperable with the existing CLI ecosystem.

Built on **Tauri 2** with React 18 + Vite + shadcn/ui + Radix + Tailwind 4. The existing `mur-agent-runtime` is bundled as a **Tauri sidecar** (child process) rather than embedded in-process, preserving zero-divergence behaviour with CLI launches. Two distribution modes are supported: **Template** (default — recipient mints fresh UUID + Ed25519 keypair on first launch) and **Clone** (opt-in `--clone-identity` — embeds a ship key, runs P0a.6 rekey on first launch with the ship key shredded immediately afterwards).

v1 builds host-only. Cross-platform releases use a documented GitHub Actions matrix template; macOS code-signing and Apple notarization are wired in but expected to be configured per-developer (mur ships the hooks, not credentials).

---

## 1. Goals & Non-goals

### Goals
- **G1** — Provide a click-to-launch desktop experience for any agent that today is created via `mur agent create`, without changing the agent's runtime semantics.
- **G2** — Replace the day-to-day CLI surface (`prompt`, `skill`, `mcp`, `perm`, `rekey`, `logs`, `stats`) with a typed GUI for the bundled agent, so non-CLI users can manage that agent.
- **G3** — Allow theming (icon + colour scheme) at export-time and at runtime, with the recipient never seeing CLI text.
- **G4** — Preserve full compatibility with `mur agent import`, `mur agent list`, `mur agent send` — the CLI ecosystem still sees and interacts with GUI-installed agents.
- **G5** — Default to security best-practice for distribution (no shared private keys; first-boot key generation) while keeping a path for personal device migration.

### Non-goals (v1)
- **NG1** — A chat window UI. The GUI is a launcher + manager only; conversational interaction stays with the commander, the CLI (`mur agent send`), or any A2A peer.
- **NG2** — Cross-compilation from a single host. v1 builds the host platform's bundle only; multi-platform releases use CI matrix.
- **NG3** — Auto-update mechanism. The bundle ID and version scheme leave room for it (P2).
- **NG4** — Multi-agent management UI. Each `.app` manages exactly one agent.
- **NG5** — User-supplied custom themes. v1 ships 5 built-in themes and bakes them into the bundle; community marketplace is P2.
- **NG6** — Remote crash reporting / telemetry to mur. Privacy default-off, with documented opt-in path for P2.

---

## 2. Decisions Summary

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| 1 | Primary role | **Pure menubar/tray launcher** (no chat window) | Matches user mental model of "click to launch the agent". Day-to-day chat goes through commander or A2A peers. |
| 2 | Platforms | **Cross-platform (mac / Linux / Win) via Tauri 2** | Native webview gives 50–80 MB binary vs Electron's 100–150 MB. Single codebase. |
| 3 | Settings scope | **Full — replaces `mur agent` CLI for that one agent** | If the GUI's purpose is to remove the need for CLI, half-replacement just makes users hop between surfaces. |
| 4 | Self-containment | **Yes — no `mur` CLI dependency on recipient machine** | Defines the export semantics: the artifact must be runnable as-is. |
| 5 | Skin/icon timing | **Hybrid** — export-time default, runtime-switchable; range = icon + colour-scheme only | Users want to change theme without re-export; full font/background-image system is YAGNI. |
| 6 | Data location | **`~/.mur/agents/<name>/`** | Standard mur path; preserves CLI interoperability; "uninstall" is a menu action, not a folder convention. |
| 7 | Cross-compile | **No, host-only; provide GH Actions matrix template** | Tauri webview is OS-bound (WKWebView / WebView2 / WebKitGTK); macOS notarization requires mac. Industry-standard pattern. |
| 8 | Identity model | **Template mode default, `--clone-identity` opt-in** | Avoids shipping shared private keys (Cisco/WD anti-pattern). Clone mode preserved for personal device migration; ship key shredded after first-launch rekey with no grace. |
| 9 | Process model | **Sidecar (child process), not in-process** | `mur-agent-runtime` already designed as standalone supervisor (running.lock, argv[0] dispatch, SIGTERM handling). Zero-divergence with CLI; crash-isolated. |
| 10 | Settings layout | **6-tab sidebar + separate Logs window** | macOS Ventura / Slack / 1Password convention; Logs as separate window matches Console.app / VS Code Output Panel pattern. |
| 11 | Frontend stack | **React 18 + Vite + shadcn/ui + Radix + Tailwind 4** | Familiar to existing mur-web team; shadcn gives a11y for free; Tailwind variables align with theme tokens. |
| 12 | Crash reporting | **Default off** (privacy floor) | Agents process sensitive prompt content; default-on remote crash dumps would be a data-exfil risk. |

---

## 3. CLI Surface

`mur-core/src/main.rs` extends the existing `Export` action's `--format` to accept a third value `gui`:

```bash
mur agent export <name> -o MyAgent.app --format gui \
    [--theme dark|light|high-contrast|solarized|cyberpunk] \
    [--icon /path/to/logo.png] \
    [--clone-identity] \
    [--skip-notarize]
```

Flags:

| Flag | Purpose | Default |
|------|---------|---------|
| `--theme` | Default theme baked into bundle (icon + colour-scheme) | `light` |
| `--icon` | Override theme's app icon with a user-supplied PNG; auto-converts to `.icns` / `.ico` / multi-size PNG | none |
| `--clone-identity` | Embed `identity.{key,pub}` + UUID; recipient inherits identity (rekeys on first launch) | off |
| `--skip-notarize` | Build unsigned `.app` (skip codesign + notarytool); useful for local testing | off (signing/notarization enabled when credential env vars set) |

**Cross-compile is not supported in v1.** Passing `--target` mismatched with the host gives a clear error referencing `docs/cookbook/multi-platform-export.md` (the GH Actions template). v1 outputs map to host:

| Host | Output |
|------|--------|
| macOS | `MyAgent.app` (Universal: x86_64 + aarch64), optionally bundled in `.zip` |
| Linux (Ubuntu 22.04+) | `MyAgent.AppImage` (built against glibc 2.35) |
| Windows 10+ | `MyAgent.exe` (NSIS installer; WebView2 evergreen bootstrap) |

`--format=bin` and `--format=pkg` are unchanged; `gui` is purely additive.

A new sibling command, `mur agent doctor [--format gui]`, runs the prerequisite checks (toolchain, system libraries, signing credentials) without performing a build. Same prereq logic is used internally as a fail-fast step inside `export --format gui`.

---

## 4. App Architecture

### 4.1 Process model

```
MyAgent.app  (Tauri 2 main process)
├─ webview                            ← React frontend, Tauri commands
├─ admin lib (in-process)             ← mur-core::agent_admin — edits ~/.mur/agents/<name>/*.yaml
├─ sidecar manager                    ← spawn/restart/SIGTERM mur-agent-runtime
│   └─ mur-agent-runtime (child)      ← unchanged from CLI mode
│       ├─ acquires running.lock
│       ├─ A2A v0.3 over Unix socket
│       └─ spawns MCP server children (process group)
└─ Tauri command IPC (typed)          ← capability/permission gated
```

Two crisp IPC paths:

- **Admin** — `Tauri command → mur-core::agent_admin → atomic YAML write`. No runtime involvement; runtime re-reads on next start. Works whether sidecar is running or not.
- **Runtime control** — `Tauri command → A2A Unix socket → runtime`. Used for status, log tail, telemetry, message send. Spawn / kill semantics use process signals on the sidecar.

### 4.2 Why sidecar, not in-process

`mur-agent-runtime` is already a BusyBox-style standalone binary (see `mur-agent-runtime/README.md`):

- argv[0] dispatch (`supervisor.rs:50`)
- `running.lock` flock + pid + UUID + card digest (`supervisor.rs:121-122`, `:307`)
- SIGTERM/SIGINT handlers flush telemetry + drop lock
- `--features=embedded-agent` already supports content-addressed extraction

Embedding it in-process would require: replacing flock with in-memory mutex, rewriting argv dispatch, rerouting signal handling, doubling the telemetry / stdio / lock cleanup paths. CLI-launch behaviour and GUI-launch behaviour would drift over time. Sidecar pattern reuses the existing binary unchanged, matches the industry standard for menubar+daemon products (Docker Desktop, Tailscale, OrbStack, Cloudflare WARP), and is the canonical Tauri 2 pattern via `tauri-plugin-shell`'s `command.spawn()` against a `bundle.externalBin` entry.

### 4.3 Restart and supervision

GUI's sidecar manager supervises the runtime with exponential backoff to avoid hot-restart loops:

```
crash 1: restart immediately
crash 2 within 60 s: wait 2 s
crash 3 within 60 s: wait 4 s
crash 4 within 60 s: wait 8 s
crash 5 within 60 s: wait 16 s, max wait 60 s
6+ crashes within 60 s: stop restarting; tray icon → red; banner "Agent crashed repeatedly. View logs."
```

Tauri `tauri-plugin-shell` does **not** implement backoff itself; the manager owns this state.

### 4.4 Process tree shutdown

Clean shutdown must terminate sidecar **and** its MCP server children:

- **macOS / Linux:** runtime calls `setpgid(0, 0)` at startup so it heads its own process group; GUI sends `kill(-pgid, SIGTERM)` (negative PID = process-group). Existing `mur-agent-runtime/src/supervisor.rs` does not yet `setpgid` — small patch in P1 plan.
- **Windows:** GUI creates a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, assigns the sidecar to the job at spawn time. When GUI quits the job closes → kernel kills the entire tree.

Without this, MCP server children become orphan zombies that survive GUI shutdown.

### 4.5 PATH augmentation for spawned MCP children

A `.app` launched from `/Applications` via Finder inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin` only — no `/opt/homebrew/bin` or `/usr/local/bin`. MCP servers installed via `brew`/`npm`/`uv` are then `command not found`. GUI explicitly augments `PATH` for the sidecar:

```rust
let augmented = format!(
    "/opt/homebrew/bin:/usr/local/bin:{}:/usr/bin:/bin:/usr/sbin:/sbin",
    std::env::var("PATH").unwrap_or_default()
);
sidecar_cmd.env("PATH", augmented);
```

VS Code, Cursor, and Claude Desktop all do this for MCP. Linux / Windows have analogous `PATH` augmentation tailored to platform conventions.

### 4.6 mur-core refactor: extract `agent_admin` library module

Today `mur-core/src/cmd/agent.rs` mixes clap parsing with admin logic. Phase 1 of the plan extracts pure functions:

```rust
// mur-core/src/agent_admin/mod.rs
pub fn perm_allow_host(name: &str, host: &str) -> Result<()>;
pub fn perm_set_mode(name: &str, scope: NetworkScope, mode: PermMode) -> Result<()>;
pub fn mcp_add(name: &str, server: McpServerConfig) -> Result<()>;
pub fn skill_add(name: &str, source: SkillSource) -> Result<()>;
pub fn prompt_set(name: &str, body: &str) -> Result<()>;
pub fn rekey(name: &str, reason: RekeyReason) -> Result<RotationAttestation>;
// … one wrapper per CLI verb
```

Both `mur` CLI and Tauri command handlers call these. clap stays in `cmd/agent.rs` as a thin shell. This is the single largest refactor required by the GUI work; it benefits the codebase regardless of the GUI shipping.

---

## 5. First-Launch Bootstrap

The bundle ships an embedded payload (tar.gz under `bundle.resources`) plus `metadata.json` declaring the agent name, display name, mode (`template` | `clone`), and theme defaults. On first launch, before spawning the sidecar:

### 5.1 Template mode (default)

Payload contents: `profile.yaml` (with `id` / `identity` fields stripped), `sys_prompt.md`, `skills/`, MCP config, theme defaults. **No `identity.key`, no UUID.**

```
1. parse metadata.json
2. resolve target = ~/.mur/agents/<name>/
3. if target exists:
     prompt "An agent named '<name>' already exists. Install as '<name>-2'?"
4. extract payload to target
5. mint Ed25519 keypair → identity.key (0600), identity.pub
6. mint UUIDv7 → write profile.yaml.id
7. write rotations.jsonl bootstrap entry
8. write profile.yaml.gui.theme = <export-time default>
9. ensure ~/.local/bin/mur_agent_<name> symlink → bundled runtime
10. spawn sidecar; wait for running.lock; tray icon → green
```

Each recipient ends up with an independent agent identity. The author's own copy is unaffected. Commander-side, each install registers as a new agent.

### 5.2 Clone mode (`--clone-identity`)

Payload contents: full `~/.mur/agents/<name>/` tree including `identity.{key,pub}` and `rotations.jsonl`. CLI emits a warning at export time:

```
WARNING: --clone-identity embeds the agent's private key into the distributable .app.
Anyone with the .app can sign rotation attestations as the original agent until first
launch rotates the key. Use only for personal device migration or fully-trusted
single-recipient transfer.
```

First-launch flow:

```
1. parse metadata.json
2. resolve target = ~/.mur/agents/<name>/
3. if target exists with same UUID:
     skip rekey, preserve current state ("re-installing same agent on same machine")
   else if target exists with different UUID:
     name conflict dialog (same as template)
   else:
     extract full payload (incl. identity.key)
     IMMEDIATELY run rekey ceremony:
       - sign RotationAttestation with ship key (old → device)
       - generate device keypair, write identity.{key,pub}
       - shred -u identity.key.prev (NO 30-day grace; ship key was always shared)
       - append attestation to rotations.jsonl, advance key_version
4. write profile.yaml.gui.theme = <export-time default>
5. symlink + spawn sidecar (as in template mode)
```

Commander-side, the device pubkey is the agent's first observed pubkey; subsequent installs by other recipients produce a `split_attestation_v1_to_v2` event (P0a.6 § M5.2) which quarantines the agent pending admin approval — the intended safety net.

### 5.3 Why bootstrap to `~/.mur/`, not the bundle

macOS App Translocation (Gatekeeper) copies `.app` launched from `~/Downloads` to a randomised read-only path on first launch. Code that tries to read agent state from inside the bundle gets unstable paths. State must live in a stable user-writable location. `~/.mur/agents/<name>/` is that location; it also makes the agent visible to `mur agent list` if the user later installs the CLI.

### 5.4 Uninstall

Tray menu → "Uninstall MyAgent…" opens a confirmation dialog. On confirm:

1. SIGTERM sidecar; wait up to 5 s for clean shutdown.
2. Remove `~/.mur/agents/<name>/`.
3. Remove `~/.local/bin/mur_agent_<name>` symlink.
4. Remove platform login-item (SMAppService unregister on macOS, etc.).
5. Quit Tauri main; user drags `.app` to Trash to remove the bundle.

---

## 6. Settings UI

### 6.1 Window layout

Settings = single window, 720 × 560 default, sidebar on the left (macOS Ventura pattern). **6 tabs**, named after CLI nouns:

| Tab | Maps to CLI | Notes |
|-----|-------------|-------|
| **Status** | `mur agent status`, `install-service`, `card`, `export`, `remove` | Running state · Start/Stop/Restart · Open at Login · Theme picker · Re-export… · Uninstall… |
| **System Prompt** | `mur agent prompt` | Monaco editor, markdown highlight, **explicit Save** button |
| **Skills** | `mur agent skill` | List + preview + Add from file… / Add from URL… / Remove |
| **MCP Servers** | `mur agent mcp` | List + add form (cmd, args, env, transport) + enable/disable + rename + prereq check |
| **Permissions** | `mur agent perm` | Network (in/out modes + host lists) · Filesystem (read/write/deny paths) · Spawn (allow/deny binaries) · Limits (cpu/mem/disk/timeout) |
| **Identity** | `mur agent rekey`, `rekey-status`, `card` | pubkey · key_version · grace · rotation history · Rotate Key… · Emergency Rekey… |

Logs and Stats are **a separate window**, opened from tray "Open Logs…". Layout: top half live tail of `<agent_home>/stderr.log` + GUI's own `gui.log`; bottom half telemetry counters (tasks completed, tokens in/out, tool calls, errors) with hourly sparkline. This matches Apple's Console.app convention and lets the user keep Logs visible while editing settings.

### 6.2 Hot-apply vs Restart-required

Industry pattern (Docker Desktop, Slack, VS Code):

| Setting class | Behaviour |
|---|---|
| Theme, Open at Login, log filter, GUI tab state | Auto-save, apply immediately |
| `sys_prompt.md` body | **Explicit Save** (text editor convention) |
| profile/perm/mcp/skill/identity edits | Auto-save to YAML; show a "Restart Required" pill next to the changed field; persistent bottom banner with **Restart Agent** button collects all pending restarts |

### 6.3 CLI-equivalent tooltips

Each control has a hover tooltip showing its CLI equivalent, e.g. on the "Allow host" row in Permissions:

```
Equivalent CLI:
mur agent perm allow-host my_agent "*.example.com"
```

Same pattern as VS Code's keybinding tooltips. Lets power users keep one mental model spanning GUI and CLI.

### 6.4 Localisation

UI strings via JSON dictionaries (per `learned-l10n-dictionary-based-no-bundle`); locale autodetect from `navigator.languages`, override in Status tab. v1 ships en + zh-TW; ja and others as community PRs.

---

## 7. Theme System

### 7.1 What's themable

| Property | Runtime-switchable |
|----------|-------------------|
| Webview colour-scheme (10 CSS variables) | ✅ |
| System tray icon | ✅ |
| Dock / taskbar icon (running-app representation) | ✅ |
| Bundle icon (Finder, Launchpad, Cmd+Tab static badge) | ❌ — export-time only |

Out of scope for v1: typography, background images, vibrancy/translucency, animation specs.

### 7.2 Schema

```json
{
  "schema_version": 1,
  "name": "cyberpunk",
  "display_name": {
    "default": "Cyberpunk",
    "zh-TW": "賽博龐克",
    "ja": "サイバーパンク"
  },
  "kind": "dark",
  "match_system": false,
  "colors": {
    "bg":             "#0d0221",
    "fg":             "#f9f9f9",
    "bg_secondary":   "#1a0e3a",
    "fg_secondary":   "#a89cc8",
    "accent":         "#ff2cd6",
    "accent_fg":      "#0d0221",
    "border":         "#3a2670",
    "success":        "#39ff14",
    "warning":        "#fce300",
    "danger":         "#ff003c"
  },
  "icons": {
    "app":                  "app.icns",
    "tray_template":        "tray-template.png",
    "tray_color":           "tray-color.png",
    "tray_running_overlay": "tray-running-overlay.png"
  }
}
```

`schema_version` lets future breaking changes be detected. `display_name` is i18n-ready from day one. Colours stay hex in v1; OKLCH migration is a P2 schema_version=2 evolution aligned with the eventual community marketplace.

### 7.3 Built-in themes (v1)

| Name | kind | Notes |
|------|------|-------|
| `light` | light | Default; macOS-friendly |
| `dark` | dark | Default dark |
| `high-contrast` | dark | **Required** for accessibility compliance — AAA contrast, prominent focus ring |
| `solarized` | dark | Classic Solarized Dark palette |
| `cyberpunk` | dark | High-contrast neon; mac users see colour tray (HIG deviation, documented) |

Plus a global **Match System** toggle (independent of theme): when on, light/dark themes are paired and the OS appearance event drives the live switch:

- macOS: subscribe to `NSWindow::theme_changed` via Tauri 2's `WindowEvent::ThemeChanged`
- Windows: watch `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme`
- Linux: `gsettings monitor org.gnome.desktop.interface color-scheme` (GNOME / KDE supported, others fall back to manual)

### 7.4 Tray icon platform conventions

| Platform | Convention | Files required |
|----------|-----------|----------------|
| macOS | Black-on-clear template image (system tints) | `tray-template@1x.png` (16×16), `tray-template@2x.png` (32×32) |
| Linux | Full-colour PNG (libayatana-appindicator3) | `tray-color-16.png`, `tray-color-22.png`, `tray-color-32.png` |
| Windows | Multi-size embedded ICO | `tray-color.ico` |

Build pipeline rejects a theme that's missing any of these for the target platform. A theme may opt to override the macOS template convention with full-colour (e.g. cyberpunk) by setting `tray_color_on_macos: true`; doc warns about HIG deviation.

### 7.5 Theme location

| Path | Purpose |
|------|---------|
| `MyAgent.app/Contents/Resources/themes/<name>/` | Read-only bundled themes |
| `~/.mur/agents/<name>/profile.yaml` `gui.theme` | User's selected theme name |

v1 does not load themes from outside the bundle. P2 may load from `~/.mur/agents/<name>/themes/<custom>/` for user-supplied themes.

### 7.6 Custom icon override

`mur agent export ... --icon /path/to/logo.png`: build pipeline auto-converts to all platform formats (`iconutil` on mac, `imagemagick` for ICO, multi-size PNG for Linux), writes into `themes/_custom/`, sets it as the bundle icon AND the runtime dock icon. Colour-scheme is inherited from `--theme` (orthogonal — "Cyberpunk colours + my company logo"). This is the only theme-asset path that requires re-export to change.

### 7.7 Build-time validation

CI / `mur agent export` rejects a theme bundle that fails any:

- WCAG AA contrast: `fg`/`bg` ≥ 4.5:1, `accent_fg`/`accent` ≥ 4.5:1, UI elements ≥ 3:1
- All asset files present at the expected paths
- PNG dimensions correct, alpha channel present where required
- `.icns` / `.ico` valid (test by parsing header)
- `theme.json` schema-valid

---

## 8. Build Pipeline

### 8.1 New workspace crate `mur-agent-gui`

```
mur/
├── mur-common/
├── mur-core/
├── mur-agent-runtime/
└── mur-agent-gui/                    ← new (workspace EXCLUDE; not built by default)
    ├── src-tauri/
    │   ├── Cargo.toml
    │   ├── tauri.conf.json           ← template, rewritten per export
    │   ├── capabilities/main.json    ← Tauri 2 permission allowlist
    │   ├── entitlements.plist        ← macOS hardened runtime
    │   ├── icons/                    ← fallback icons
    │   ├── themes/                   ← 5 built-ins
    │   └── src/
    │       ├── main.rs               ← Tauri main; bootstrap; sidecar manager
    │       ├── commands/             ← Tauri commands (admin + runtime control)
    │       └── theme.rs              ← theme loader + appearance subscriber
    ├── ui/
    │   ├── package.json
    │   ├── vite.config.ts
    │   ├── tailwind.config.ts        ← shares design tokens with mur-web
    │   └── src/
    │       ├── App.tsx
    │       ├── tabs/
    │       └── lib/
    └── README.md
```

`mur-agent-gui` is **excluded** from the workspace `members` list. Default `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2. Only `mur agent export --format gui` and the dedicated CI job build it.

### 8.2 `mur agent export --format gui` phases (with telemetry spans)

| # | Phase | Detail | Typical duration |
|---|-------|--------|------------------|
| 1 | `prereq_check` | Verify cargo, node, npm, tauri-cli, platform libs, signing creds. Same logic as `mur agent doctor`. Fail-fast with actionable hints. | ~30 ms |
| 2 | `prepare_payload` | Build agent payload tarball (template-mode strips identity + UUID; clone-mode preserves all). Reuse `mur-agent-runtime/src/export/pkg.rs` internals. | ~100 ms |
| 3 | `prepare_theme` | Resolve theme dir; if `--icon` given, transcode to platform icon formats. Run WCAG validation. | ~500 ms |
| 4 | `rewrite_tauri_conf` | Generate `tauri.conf.json` from template: productName, identifier (`run.mur.agent.<safe-name>`), version, bundle.icon, externalBin, resources, capability. | ~10 ms |
| 5 | `build_sidecar` | **mac:** build x86_64 + aarch64, lipo to universal. **else:** single target. Output to `src-tauri/binaries/mur-agent-runtime-<target-triple>`. | 60–120 s cold, 15–30 s warm |
| 6 | `build_frontend` | `npm ci && npm run build` → `ui/dist/` (per-tab code-split). | 30–60 s cold, 10–20 s warm |
| 7 | `tauri_build` | `cargo tauri build --target <target> --bundles <…>`. | 60–180 s cold, 30–90 s warm |
| 8 | `codesign` | macOS: sign sidecar (`--options runtime --timestamp`), then sign outer `.app --deep`. Win: signtool on `.exe`. Linux: noop. | ~10 s |
| 9 | `notarize` | macOS only: `notarytool submit --key`, poll for completion. Skipped if `--skip-notarize`. | 300–900 s queue |
| 10 | `staple` | macOS: `xcrun stapler staple` on `.app` (and `.dmg` if present). | ~5 s |
| 11 | `assess` | macOS: `spctl --assess --type execute --verbose` to verify; CI fails on rejection. | ~1 s |
| 12 | `package` | mac: zip the signed `.app`. linux: AppImageTool. win: NSIS. | ~5–30 s |
| 13 | `move_to_out` | Copy to user-specified `-o` path; print success. | <1 s |

Each phase emits an OpenTelemetry span; failures attach the phase name to make `mur agent doctor` and post-mortem easy.

### 8.3 macOS specifics

**Bundle identifier:** `run.mur.agent.<safe-name>` (reverse-DNS, lowercased, `[a-z0-9-]` only).

**Universal binary:** `--target universal-apple-darwin`. The two arch-specific runtime binaries must both exist at `src-tauri/binaries/mur-agent-runtime-{x86_64,aarch64}-apple-darwin` (Tauri lipo's at bundle time). Codesign is applied to the universal binary, not per-arch.

**Hardened Runtime entitlements** (`src-tauri/entitlements.plist`):

```xml
<key>com.apple.security.cs.allow-jit</key><true/>
<key>com.apple.security.cs.allow-unsigned-executable-memory</key><true/>
<key>com.apple.security.cs.disable-library-validation</key><true/>
<key>com.apple.security.cs.disable-executable-page-protection</key><true/>
<key>com.apple.security.cs.allow-dyld-environment-variables</key><true/>
<key>com.apple.security.network.client</key><true/>
<key>com.apple.security.network.server</key><true/>
<key>com.apple.security.files.user-selected.read-write</key><true/>
```

`disable-executable-page-protection` is essential: Node.js / V8 JIT inside MCP children fails to spawn without it.

**Codesign command** (every binary, every framework):

```
codesign --force --options runtime --timestamp \
         --sign "Developer ID Application: <Team> (<TeamID>)" \
         --entitlements entitlements.plist \
         <path>
```

Missing `--options runtime` → notarization rejected. Missing `--timestamp` → signature expires. Inner-to-outer order: sidecar first, then `.app --deep`.

**Notarization:** API key (`xcrun notarytool submit --key key.p8 --key-id <id> --issuer <iss>`). Apple ID + app-specific password is deprecated; v1 uses API key only.

**MACOSX_DEPLOYMENT_TARGET = 12.0**, not 10.15. Required to use `SMAppService.loginItem` (the modern login-item API). Tauri 2's `tauri-plugin-autostart` uses this on 13+ and falls back gracefully on 12.

**Distribution format:** ZIP of signed `.app` (not DMG). DMG requires designed background, separate stapling, and signing; ZIP relies on the staple inside `.app` and is friction-free. P2 may add DMG.

### 8.4 Windows specifics

**Bundle:** NSIS (not WiX/MSI) — `--bundles nsis`. Avoids the WiX toolchain on builders.

**WebView2:** `bundle.windows.webviewInstallMode = { "type": "downloadBootstrapper" }`. ~150 KB stub that downloads the runtime if absent. P2 may add a fixed-version WebView2 option for air-gapped environments.

**Code signing:** `signtool sign /tr <TSA> /td sha256 /fd sha256 /a MyAgent.exe`. v1 documents OV cert (~$80–200/yr, SmartScreen warning until reputation accumulates) and EV cert (~$250–400/yr + hardware token, immediate reputation). Spec recommends OV for v1, EV for public release.

**Process-tree kill:** Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` assigned to sidecar at spawn.

### 8.5 Linux specifics

**Build host:** `ubuntu-22.04` (glibc 2.35). Building on newer Ubuntu yields AppImages that won't run on 22.04. The OLDEST supported target dictates the build host.

**Required system libs at build time:** `libwebkit2gtk-4.1-dev`, `libsoup-3.0-dev`, `libayatana-appindicator3-dev`.

**Runtime dependency:** `libfuse2` on the user's host. Documentation surfaces `sudo apt install libfuse2` for Ubuntu users.

**GNOME tray pre-flight check:** First-launch detects `XDG_CURRENT_DESKTOP=GNOME` and a missing AppIndicator extension → modal dialog with install link to extensions.gnome.org. Falls back to window-only mode if the user opts out.

### 8.6 GitHub Actions matrix template

`scripts/templates/agent-export-multi-platform.yml`:

```yaml
name: Build agent app
on: { workflow_dispatch: {} }
jobs:
  build:
    strategy:
      matrix:
        os: [macos-14, ubuntu-22.04, windows-2022]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { toolchain: stable, targets: x86_64-apple-darwin,aarch64-apple-darwin }
        if: runner.os == 'macOS'
      - uses: actions/setup-node@v4
        with: { node-version-file: 'mur-agent-gui/ui/.nvmrc' }
      - if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libsoup-3.0-dev \
                                  libayatana-appindicator3-dev libfuse2
      - uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            mur-agent-gui/src-tauri/target
            mur-agent-gui/ui/node_modules
          key: ${{ runner.os }}-tauri-${{ hashFiles('**/Cargo.lock','**/package-lock.json') }}
      - run: cargo install --git https://github.com/mur-run/mur mur-core --locked
      - run: mur agent import ./my-agent.murpkg
      - env:
          MUR_APPLE_NOTARY_KEY:    ${{ secrets.APPLE_NOTARY_KEY }}
          MUR_APPLE_NOTARY_KEY_ID: ${{ secrets.APPLE_NOTARY_KEY_ID }}
          MUR_APPLE_NOTARY_ISSUER: ${{ secrets.APPLE_NOTARY_ISSUER }}
        run: mur agent export my-agent -o dist/MyAgent --format gui
      - uses: actions/upload-artifact@v4
        with:
          name: MyAgent-${{ matrix.os }}
          path: dist/
```

Documented at `docs/cookbook/multi-platform-export.md`.

### 8.7 Toolchain pinning

| File | Purpose |
|------|---------|
| `rust-toolchain.toml` | Stable Rust channel for all crates |
| `mur-agent-gui/ui/.nvmrc` | Node 20 LTS |
| `mur-agent-gui/ui/package-lock.json` | npm dep lockfile (use `npm ci`, never `npm install`) |
| `mur-agent-gui/src-tauri/Cargo.toml` | Tauri 2.x exact-pinned via `=` selectors for Tauri-* crates |

### 8.8 Build host requirements

| Target | Required host | Notes |
|--------|---------------|-------|
| macOS Universal | **Apple Silicon strongly preferred** | x86_64 host can build `--target x86_64-apple-darwin` only without cross-toolchain setup |
| Linux AppImage | Ubuntu 22.04 | Older host = wider compat; newer host = narrower |
| Windows | Windows 10/11 | MSVC toolchain; cross from Linux via mingw not supported |

### 8.9 Privacy defaults

- **No remote crash reporting.** Tauri-plugin-sentry not bundled. Logs stay local at platform-conventional paths:
  - macOS: `~/Library/Logs/MyAgent/gui.log`
  - Linux: `~/.local/state/MyAgent/gui.log`
  - Windows: `%LOCALAPPDATA%\MyAgent\Logs\gui.log`
- **No update telemetry.** v1 has no auto-update; bundle ID + SemVer leave room for `tauri-plugin-updater` in P2.
- **Sidecar telemetry stays local.** Existing `~/.mur/agents/<name>/telemetry/*.jsonl` writes are unchanged; no commander-side change required.

### 8.10 Ergonomics

`mur agent doctor [--format gui]` — independent prereq check command (no build), prints actionable install hints. Shared logic with `prereq_check` phase.

`mur agent gc [--older-than 7d]` — sweeps `~/.cache/mur/export-gui/` staging dirs. v1 manual; P2 may auto-clean.

---

## 9. Security Considerations

| Area | Mitigation |
|------|------------|
| Shipped private keys (Cisco/WD anti-pattern) | Template mode default — never embeds `identity.key`. Clone mode requires explicit opt-in with CLI warning. |
| TOFU race in clone mode (attacker registers first) | Commander's existing split-attestation detection (P0a.6 § M5.2) quarantines on conflict. Documented as expected behaviour. |
| Crash dumps containing sensitive prompt content | No remote crash reporting in v1. P2 opt-in only. |
| macOS Gatekeeper bypass on unsigned builds | `--skip-notarize` is documented as testing-only; CI fails closed if creds set but signing fails. Production export is signed + notarized + stapled. |
| Tauri IPC abuse (untrusted webview content) | All commands gated by Tauri 2 capability JSON. Allowed origins locked to `tauri://localhost` / `https://tauri.localhost`. CSP disallows remote scripts. |
| MCP child process isolation | Process group + Job Object ensure children die with sidecar. Unrelated to the in-process / sidecar choice — both have to do this. |
| App Translocation breaking bundle reads | All persistent state lives at `~/.mur/agents/<name>/`, never read from inside the bundle after startup. |
| Sidecar binary tampering | Codesigned by same identity as outer `.app`; codesign --deep verifies on every launch. |
| Rust supply chain | `Cargo.lock` committed, `--locked` builds in CI, no Dependabot auto-merge for transitive bumps. SBOM hook reserved for P2 via `MUR_GENERATE_SBOM=1`. |

---

## 10. Risks & Open Questions

| ID | Risk | Mitigation / Decision Needed |
|----|------|------------------------------|
| R1 | Tauri 2's macOS sidecar codesign tooling has rough edges; some teams report needing custom post-build scripts. | First-pass implementation accepts manual signing scripts; bug surface should be documented in P1 plan. |
| R2 | shadcn/ui evolves rapidly; component API churn could affect maintenance. | Pin shadcn registry to a tag; track upstream releases in P2 ergo task. |
| R3 | macOS Gatekeeper assessment may regress on older OS versions when `MACOSX_DEPLOYMENT_TARGET=12`. | CI matrix should include macOS 12 runners alongside macOS 14. |
| R4 | Linux AppImage ergonomics on GNOME without AppIndicator extension are poor. | First-launch dialog explains; document in `docs/cookbook/linux-tray.md`. |
| R5 | Windows EV cert cost is a real friction for hobby use. | Document OV vs EV trade-off; ship without EV in v1; allow `--skip-notarize`-equivalent on Windows for unsigned builds. |
| R6 | Universal binary size on macOS doubles vs single-arch. | Acceptable trade-off (50–80 MB → 70–110 MB). Alternative: ship two archives (x86_64 + aarch64), leave to P2 if size becomes user-visible pain. |
| OQ1 | Should the Status tab include a "Switch to commander" link / button that opens the user's commander app? | Defer; commander discovery isn't standardised yet. |
| OQ2 | Should template-mode `.app` carry the AUTHOR's signature so recipients can verify "this came from X"? | Defer to P2 (signed-template mode). v1 relies on macOS codesign + Apple notarization for code identity. |
| OQ3 | Should `mur agent export --format gui` allow `--update-channel` so installed apps can pull updates from a specific stream? | Defer — needs P2 auto-update infra. |

---

## 11. Phased Implementation Plan (estimate)

| Phase | Scope | Est. LOC | Est. duration |
|-------|-------|----------|---------------|
| **P1.0** | Refactor `mur-core/src/cmd/agent.rs` admin verbs into `mur-core/src/agent_admin/` library module; CLI becomes thin shell. Existing tests remain green. | +800 / -700 | 2–3 days |
| **P1.1** | Add `setpgid` to `mur-agent-runtime/supervisor.rs`; add `mur agent doctor` command sharing prereq check logic with future export pipeline. | +300 | 1 day |
| **P1.2** | Scaffold `mur-agent-gui` crate (Tauri 2 main + Vite frontend + 5 themes). Sidebar layout with 6 tabs + Logs window. No real backend wiring yet — stub Tauri commands. | +3500 | 5–7 days |
| **P1.3** | Wire admin lib via Tauri commands (Status, System Prompt, Skills, MCP, Permissions, Identity tabs operational). | +1500 | 4–5 days |
| **P1.4** | Wire sidecar manager: spawn, monitor, restart-with-backoff, `setpgid` / Job Object kill semantics, PATH augment. Logs window live tail. | +900 | 3–4 days |
| **P1.5** | Theme system: schema + WCAG validator + appearance subscriber + Tailwind CSS-vars wiring + tray icon swap + dock icon swap. | +700 | 2–3 days |
| **P1.6** | Bootstrap on first launch: template mode (mint key + UUID), clone mode (rekey ceremony). Tests against synthetic embedded payloads. | +600 | 2 days |
| **P1.7** | `mur agent export --format gui` pipeline (`mur-core/src/agent/export_gui.rs`): payload prep, `tauri.conf.json` rewrite, sidecar build, frontend build, `cargo tauri build`, codesign + notarize + staple + assess. | +1200 | 4–5 days |
| **P1.8** | GitHub Actions matrix template + cookbook docs. End-to-end test on all three OS runners. | +300 | 2 days |
| **P1.9** | E2E tests: `scripts/e2e/p1-export-gui.sh` produces artifacts on each OS; spawns the resulting app; asserts `running.lock` appears; sends an A2A message via the GUI. | +400 | 2 days |

**Total: ~10 000 LOC, ~5–6 weeks** for one engineer working full-time. Parallelisable across frontend (P1.2/P1.3) and backend (P1.4/P1.6/P1.7) streams to ~3–4 weeks elapsed.

---

## 12. References

- `mur-core/src/main.rs:967-977` — current `Export` action enum (where `gui` format is added).
- `mur-core/src/cmd/agent.rs:1283-1335` — current `cmd_export` dispatch (template/clone branch added here).
- `mur-agent-runtime/README.md` — runtime architecture overview.
- `mur-agent-runtime/src/supervisor.rs:50,121-122,307` — argv[0] dispatch + flock + lock writeout (sidecar-mode unchanged).
- `mur-agent-runtime/src/export/pkg.rs` — payload tarball helper reused by GUI export.
- `docs/superpowers/specs/2026-04-22-murmur-p0-agent-runtime-design.md` — P0a foundation spec.
- `docs/superpowers/specs/2026-04-23-murmur-fleet-architecture-design.md` — P0a.5 fleet / TCP+Noise / commander bridge.
- `docs/superpowers/specs/2026-04-24-murmur-agent-rekey-design.md` — P0a.6 rekey + RotationAttestation (clone-mode bootstrap reuses this exactly).
- Tauri 2 docs — sidecar (`bundle.externalBin`), capability/permission system, `tauri-plugin-shell`, `tauri-plugin-autostart`.
- Apple Developer docs — `notarytool`, `SMAppService.loginItem`, Hardened Runtime entitlements.
- WCAG 2.2 AA contrast ratios (theme validation).
- `learned-l10n-dictionary-based-no-bundle` skill — basis for GUI string i18n.
