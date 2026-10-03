# MUR Agent Package & Two-Surface Architecture

**Date:** 2026-05-20
**Status:** Implemented (as-built sync). Sections 1–16.6 describe the shipped `mur-common::muragent` design; §16.7 and §17 are future work and are not implemented.
**Owner:** david
**Supersedes:** `2026-04-29-mur-agent-gui-export-design.md` (per-agent `.app` as default export artifact — entirely replaced; no migration)
**Builds on:** `2026-05-11-mur-hub-companion-design.md` (MUR Hub desktop surface); `2026-05-18-commander-feedback-wire-protocol-design.md` (Signal envelope, the runtime channel between surfaces)
**Related:** `2026-05-07-b1-runtime-enforcement-design.md`, `2026-04-29-model-registry-and-secret-refs-design.md`, `2026-05-09-mur-agent-c7-slack-bridge-design.md`, `2026-05-08-mur-agent-c6-idle-triggers-design.md`

## 1. Problem

The MUR ecosystem has two products that both instantiate "agents" today, but with no shared portable identity unit:

1. **MUR Hub** (`mur-hub-gui`, Tauri 2) — desktop UI surface. Per-agent windows, companion pet (drag to desktop), voice (D1), per-agent Dock icons. Local-first, single user.
2. **MUR Commander** (`mur-commander` workspace, separate repo at `~/Projects/mur-commander`, version line v0.10.x independent of mur v2.13.x) — chat/automation surface. Slack / Telegram / Discord gateway, workflow engine, MCP plugins, sub-agents, Jira, programs, supervisor. Daemon, multi-user via chat platforms.

Both products are mature, both ship independently, both have their own CLI (`mur` vs `murc`), distribution channel (`brew install --cask mur-hub` vs `brew install mur-run/tap/mur-commander` + Docker), and release cadence. They are **brand siblings**, not one product.

Today, agents live in `~/.mur/agents/<name>/` and the export path produces per-agent macOS `.app` bundles (via `mur agent export <name> --gui` — see superseded spec). Three problems with the current export path:

1. **Code signing is per-agent on macOS.** Without an Apple Developer ID, recipients hit Gatekeeper. With one, every export costs a `codesign` + `notarytool` round-trip (1–3 min each). Author-side burden does not scale, and exposes mur to revocation risk if we were to sign on users' behalf.
2. **Per-agent build is slow.** Each export runs `cargo build -p mur-agent-runtime` plus `tauri build` — minutes, not seconds. The result is a 50–100 MB self-contained bundle. Most of those bytes are duplicate runtime.
3. **Architecturally anomalous.** No other AI / automation product in the field (Slack, Discord, Cursor, Raycast, Ollama, LM Studio, Claude Desktop, ChatGPT Desktop, Shortcuts.app, Automator) ships a per-configuration signed app bundle. The universal pattern is **one signed host + data files for each configuration**.

The fourth, separate problem — addressed by the same artifact — is that **Hub and Commander have no shared way to describe an agent.** Each product has its own internal representation; an author who wants their agent to be reachable from Slack (Commander) AND have a desktop window (Hub) must duplicate the setup. There is no portable "agent" unit a user can hand to another user.

This spec defines a single artifact — **`.muragent`** — that solves all four problems: (1) data-only, no signing per export, (2) seconds to produce, (3) industry-standard host+data pattern, (4) consumable by both Hub and Commander surfaces, with surface-specific configuration carried in optional manifest blocks.

### 1.1 No backwards compatibility

The user has confirmed no production users of either the existing `.app` export or the `.murpkg` v1 format. This spec therefore defines v2 as a clean break: no migration scanners, no `.murpkg` v1 auto-upgrade, no per-agent `.app` deprecation timeline. The old surface is referenced for context only; nothing depends on it continuing to work.

## 2. Goals & Non-Goals

### Goals (v1)

- A single portable artifact (`.muragent`) consumable by **both** Hub and Commander surfaces, signed once by the author.
- Surface-agnostic agent identity: `~/.mur/agents/<slug>/`, Ed25519 keypair, A2A v0.3 — already exists; this spec extends it.
- Recipients need no Apple Developer ID, no signing tooling, no Gatekeeper exception ritual to install.
- External API surface (`muragent-<slug>://share?...` URL scheme, "Send Selection to <Agent>" Services menu, drag-drop targets) preserved across Hub surface (Commander uses chat platforms, not URL schemes).
- Cross-platform: macOS, Windows, Linux all use the same `.muragent` data file; Hub generates platform-native OS identity stubs (`.app` / `.lnk` / `.desktop`).
- Author-side signing via DSSE envelope over in-toto v1 Statement using the existing agent Ed25519 identity (no new crypto stack).
- Forward-compatible manifest format: mur-issued signature slot fits as a second DSSE envelope entry; no schema change needed when V2 trust badge ships.
- Sidecar lifecycle decoupled from Host UI lifecycle — agents run via launchd / systemd / Run registry whether or not Hub is open. Commander has its own daemon lifecycle.
- Shared local trust store at `~/.mur/trust/` consumed by both surfaces. Trust an author once, both Hub and Commander honour it.
- Preserve the existing per-agent `.app` pipeline as an `--standalone` escape hatch for authors with their own Apple Developer ID (Hub-only path).

### Non-Goals (v1, explicitly deferred)

- **Backwards compatibility with `.murpkg` v1 or the per-agent `.app` format.** Confirmed: no users to migrate. v2 is a clean break.
- **`mur` ↔ `murc` CLI unification.** The two CLIs stay separate; each consumes `.muragent` independently. A unified CLI is a future product decision, not in this spec.
- **mur-issued trust badge** (Phase 1 of the trust roadmap). V1 reserves the manifest slot, UI region, and root-of-trust pubkey, but performs no verification and issues no badges. Earliest opportunity: V2, gated on Pro subscription + public Agent Directory shipping first.
- **Public Agent Directory.** A separate initiative; sequenced after this spec ships.
- **`--bundle-with-host` super-installer** that ships Hub + agent together in one `.dmg`. The use case is real but folds back into the standalone signing problem we are explicitly avoiding.
- **Enterprise on-premise signing service** (Phase 2 of trust roadmap).
- **Flatpak / Snap / Mac App Store / Microsoft Store** distribution surfaces.
- **NSServices-equivalent on Linux** (no standardized cross-DE mechanism).
- **Commander-side stub generation.** Commander is a daemon and uses chat platform integrations for surfacing — it doesn't create Dock icons or Start Menu entries. Stubs are Hub-only artifacts.
- **Blockchain-anything.** Not on the roadmap, ever.

## 3. Architecture Principles

### 3.1 Sidecar lifecycle is decoupled from Host UI lifecycle

**This is the load-bearing principle of the new model.** It must hold even when other choices change.

- An agent's "body" is its `mur-agent-runtime` sidecar process, supervised by `launchd` (macOS), `systemd --user` (Linux), or the Windows Run registry (Windows).
- The sidecar starts at login, runs whether or not Hub is open, and survives Hub crashes.
- Background features — C6 idle triggers, C7 Slack bridge, D1 voice notifications, A2A, scheduled tasks — live in the sidecar and require no UI to be open.
- Hub UI is a **consumer** of sidecar events, not an owner. It can be killed and relaunched freely.
- OS notifications, TTS, system tray actions are delivered by the sidecar directly via OS APIs, not via Hub.

> **Why this is in §3 and not buried later:** the old per-agent `.app` model bound the sidecar lifecycle to `NSApplication`'s run loop. If we don't make decoupling an architectural rule, future contributors will reflexively put background logic in Hub and we will re-create the old failure mode in V3.

> **Migration from current code:** the existing `mur-gui-core/src/sidecar.rs` implements a Hub-spawns-children model where Hub supervises sidecar processes with exponential backoff and kills them on shutdown. This code is the **current production behavior** and is incompatible with the launchd/systemd ownership model defined here. Migration plan: M-export-5 rewrites sidecar lifecycle to use OS init systems. During the transition (M-export-1 through M-export-4), Hub continues to spawn sidecars as before; the launchd/systemd wiring is a leaf-level change in M-export-5 that swaps the supervisor without changing any other module. The decoupling principle is the target architecture — the existing child-process model is a v0 implementation that predates this spec and is explicitly superseded by it.

> **Expression engine location:** the Hub Companion Design places the pet ExpressionStateMachine and event bus (`tokio::sync::broadcast`) inside Hub. Under the decoupled model (§3.1), background features (C6 idle triggers, C7 Slack bridge, D1 voice) must function without Hub. This requires moving the expression evaluation engine into `mur-agent-runtime` (the sidecar), with Hub consuming expression state via the per-agent IPC channel (§5.3) when it is open. The expression engine migration is scoped to M-export-5 alongside the sidecar lifecycle change. Until then, the pet is dormant when Hub is closed — an acceptable v1 limitation documented in release notes.

### 3.2 OS identity is data, not a binary

Per-agent OS-level identity (bundle ID, URL scheme, NSServices, Dock icon, file association, Start Menu entry) is expressed entirely through **OS-native configuration files** — `Info.plist`, Windows registry keys, freedesktop `.desktop` files. None of it requires per-agent compiled code.

This is the same trick Chrome uses for PWAs: a signed parent app (Chrome / Hub) generates locally-created stub bundles whose Info.plist / registry entries carry the per-instance identity. Because the stubs are generated by signed code on the local machine, they never receive a quarantine `xattr` / Mark-of-the-Web flag, so Gatekeeper / SmartScreen never gates them.

### 3.3 Mur never signs user-authored content

Mur signs exactly two things:
1. The Hub app binary (`MUR Agent Host.app` / `.exe` / `.AppImage`) — signed once per release with mur's own Developer ID.
2. (V2 only, deferred) The mur-issued trust badge over a Pro user's `.muragent` manifest, after content review.

We **never** sign user-authored code or wrap a user-authored payload inside a mur-signed binary. This bounds our legal and revocation exposure: a malicious user can lose their own trust badge (V2) but cannot revoke mur's macOS Developer ID for everyone.

### 3.4 `.muragent` contains no executable code

A `.muragent` is a tarball of YAML + images. No `.so` / `.dylib` / `.dll` / `.exe` entries. No tar entries with the execute bit. No `command:` field pointing to a path inside the tarball. MCP servers must use system-resolvable commands (`uvx`, `npx`, `docker`, etc.) so the package itself never carries running code.

### 3.5 macOS persistence integrates with Login Items UI

On macOS 13+ (Ventura), Apple introduced `SMAppService` and the "Login Items & Extensions" section of System Settings. Items that an app schedules at login MUST surface there in a way the user can disable; hand-written `~/Library/LaunchAgents/*.plist` files without proper grouping appear as orphaned entries.

For the long-term ideal path (SMAppService-native), the agent's runtime binary would live inside `MUR Agent Host.app/Contents/Helpers/` and be registered via `SMAppService.agent(plistName:)`. This conflicts with the existing BusyBox-style `mur_agent_<name>` symlink architecture in `~/.mur/bin/` and would require restructuring out of scope for v1.

**v1 compromise:** continue writing `~/Library/LaunchAgents/run.mur.agent.<slug>.plist`, but **always include `AssociatedBundleIdentifiers = ["run.mur.host"]`** so the entries group correctly under "MUR Agent Host" in Login Items. v2 may move the runtime inside the Host bundle on macOS only, behind a build flag, to enable full `SMAppService` integration. Document both paths in `mur agent doctor` output so users can see which the system is using.

References: [SMAppService docs](https://developer.apple.com/documentation/servicemanagement/smappservice); [Apple DTS thread 750528 — non-bundled binaries fall back to legacy plist](https://developer.apple.com/forums/thread/750528); [theevilbit on SMAppService](https://theevilbit.github.io/posts/smappservice/).

### 3.6 Per-agent identity is OS-level data, single source of truth

The slug-derived reverse-DNS identity (`run.mur.agent.<slug>`) is the **canonical key** used across every OS surface: bundle ID, AUMID, StartupWMClass, IPC socket path, file-system home. The slug is fixed at first export and never changes for a given agent — this is what enables stable taskbar/Dock grouping, single-instance IPC, and "same agent reinstalled = same identity" semantics. See §5.0 for the full pinning table.

Slug renames require a fresh export (new uuid) and a clean import — there is no in-place rename path. This trades a small UX cost for OS-level identity stability that is required for Wayland app_id, Windows AUMID and macOS LSHandlers to behave correctly.

### 3.7 Trust failures are fatal, not advisory

There is no "Continue anyway" button on signature, integrity, or revocation failures. The single click-through path in the entire system is the §7.2 first-time-author prompt — and that is for a *valid* signature from an *unknown* author, not for an invalid signature. See §7.5 for the full rationale (Cydia case study). This principle is non-negotiable because relaxing it eliminates the security value of every other choice in §6 and §7.

### 3.8 Two surfaces, one identity

`.muragent` is the **canonical agent package for the entire MUR ecosystem** — Hub and Commander both consume it, neither owns it. An agent has exactly one Ed25519 identity, one slug, one set of capabilities; the manifest carries optional surface-specific configuration blocks (`hub:`, `commander:`) that each surface reads selectively.

Concrete consequences:

- A `.muragent` produced from a Hub-managed agent installs cleanly into Commander (Commander surface picks up `profile:`, `mcp_servers:`, `commander:` block; ignores `hub:`).
- Same agent exported and given to a recipient who has only Commander → it runs on Commander as a chat-platform agent; if recipient later installs Hub, the same agent appears in the Dock.
- Trust accrues by **author identity**, not by surface. Trusting an author in Hub means Commander also trusts them and vice versa (shared `~/.mur/trust/` — §7.1).
- Signal protocol (`mur-common::Signal`, frozen 2026-05-18) is the **runtime** feedback channel between surfaces, orthogonal to the install-time `.muragent` artifact. See §16 for how the two pieces compose.

This principle is the reason for renaming the artifact from "MUR Hub package" to "MUR Agent Package" — Hub is one consumer; the package is a property of the ecosystem.

## 4. Three-Layer Runtime Topology

```
┌─────────────── launchd / systemd --user / Run registry ────────────────┐
│                                                                        │
│   mur_agent_coach  ─── runtime sidecar (~30 MB) ───┐                    │
│   mur_agent_alice  ─── runtime sidecar (~30 MB) ───┤                    │
│   mur_agent_bob    ─── runtime sidecar (~30 MB) ───┤                    │
│                                                    │                    │
│                       A2A v0.3 over local socket   │                    │
└────────────────────────────────────────────────────┼────────────────────┘
                                                     │
   ┌─── MUR Agent Host.app ── single Tauri instance ─┘ (open on demand)
   │       ├── window: Coach
   │       ├── window: Alice
   │       └── pet:    Bob
   │
   └─── invoked by stubs in ~/Applications/MUR-Agent-*.app
                                ~\Start Menu\Programs\MUR Agents\*.lnk
                                ~/.local/share/applications/run.mur.agent.*.desktop
```

Three independent layers:

| Layer | Lifetime | Owner |
|---|---|---|
| **Runtime sidecars** | Per agent. launchd-supervised; runs at login → logout. | Existing `mur-agent-runtime` crate, unchanged. |
| **Host UI** | Single Tauri instance, opened on demand. Per-agent IPC channels (§5.3) keyed on slug let stub launches reach the right agent window. | `mur-hub-gui` (already in development per hub-companion design). |
| **OS identity stubs** | Per agent. Created locally by Hub on import; identifiers pinned to `run.mur.agent.<slug>` reverse-DNS form (§5.0). Self-regenerate on Host upgrade (§5.4). | New: created and torn down by Hub. |

Memory cost vs. status quo:
- **Status quo:** N agents × ~150 MB per-agent `.app` (UI always loaded) = 450 MB for 3 agents.
- **New model:** N × ~30 MB sidecars + 1 × ~150 MB Host UI (only while open) = 240 MB peak, 90 MB when Hub closed.

## 5. Per-Platform Stub Format

### 5.0 Per-agent identity pinning (cross-platform invariant)

Every per-agent identifier MUST be derived deterministically from the slug, using **reverse-DNS form `run.mur.agent.<slug>`** across every OS surface. This is what enables IPC keying, scheme claim correctness, taskbar grouping, and the "same agent reinstalled = same identity" guarantee.

| Surface | Value (for slug = `coach`) |
|---|---|
| macOS `CFBundleIdentifier` | `run.mur.agent.coach` |
| macOS `StartupWMClass` (n/a) / window `app_id` | — |
| Windows AUMID (`SetCurrentProcessExplicitAppUserModelID`) | `run.mur.agent.coach` |
| Linux `.desktop` file ID | `run.mur.agent.coach.desktop` |
| Linux `StartupWMClass=` and Wayland `app_id=` | `run.mur.agent.coach` |
| URL scheme | `muragent-coach://` |
| Per-agent IPC channel | `~/.mur/agents/coach/ipc.sock` (macOS, Linux), `\\.\pipe\mur-agent-coach` (Windows) |
| File system home | `~/.mur/agents/coach/` |

Slug sanitisation: lowercase, `[a-z0-9-]` only, no leading/trailing dash, length ≤ 32. Reject slugs that would collide with reserved system names (`com`, `aux`, `nul` on Windows; `.` `..` on all). Slug is locked at first export and cannot change without exporting a fresh agent (this preserves AUMID stability for taskbar grouping).

### 5.1 Per-platform stub table

| | macOS | Windows | Linux |
|---|---|---|---|
| **Stub location** | `~/Applications/MUR-Agent-<Slug>.app/` | `%APPDATA%\Microsoft\Windows\Start Menu\Programs\MUR Agents\<Slug>.lnk` | `~/.local/share/applications/run.mur.agent.<slug>.desktop` |
| **Per-agent URL scheme** | Info.plist `CFBundleURLSchemes = ["muragent-<slug>"]` | `HKCU\Software\Classes\muragent-<slug>` registry tree | `MimeType=x-scheme-handler/muragent-<slug>` in `.desktop` |
| **Explicit scheme claim** | `LSSetDefaultHandlerForURLScheme(scheme, bundle_id)` after `lsregister -f` | `IApplicationAssociationRegistration` per-user default | `xdg-mime default run.mur.agent.<slug>.desktop x-scheme-handler/muragent-<slug>` |
| **NSServices / context menu** | Info.plist `NSServices` (3 entries: text / URL / image; `serviceShare:` selector) | Optional shell context menu via `HKCU\Software\Classes\*\shell\SendTo<Slug>` (v1 skipped) | None (no cross-DE standard; v1 skipped) |
| **Per-agent Dock / taskbar icon** | `Contents/Resources/Icon.icns` (1024×1024) | `.lnk` IconFileName field; **AUMID set by launched process** | `.desktop` `Icon=run.mur.agent.<slug>` + `~/.local/share/icons/hicolor/512x512/apps/<slug>.png` |
| **Per-agent launcher** | `Contents/MacOS/<Slug>` = copy of `mur-agent-launcher` binary, ad-hoc resigned in place. **Calls `execv` directly** on the host binary at the path recorded in `~/.mur/host_path` (NOT `open -b`). | `.lnk` target = direct path to `MUR Agent Host.exe --agent <slug>` (no shim binary needed; Host sets AUMID itself) | `.desktop` `Exec=mur-agent-host --agent <slug> %u` (no shim binary) |
| **Re-registration hook** | `lsregister -f <stub>` + `/System/Library/CoreServices/pbs -update` for NSServices + `LSSetDefaultHandlerForURLScheme` | `IApplicationAssociationRegistration::SetAppAsDefault` | `update-desktop-database ~/.local/share/applications/` + `xdg-mime default` (never edit `mimeapps.list` directly; never write to deprecated `~/.local/share/applications/mimeapps.list`) |
| **`.desktop` completeness** | n/a | n/a | MUST include `StartupWMClass=run.mur.agent.<slug>`, `StartupNotify=true`, reverse-DNS file ID |
| **Signed by mur?** | No — stub is locally generated by Hub (already signed), no quarantine xattr → Gatekeeper does not gate. Launcher is ad-hoc resigned in place. | No — `.lnk` files do not get MOTW when created by an unflagged process. Host `.exe` reputation carries through the shortcut (SmartScreen is target-keyed, not shortcut-keyed). | No — Linux has no equivalent gate. |

### 5.2 macOS launcher binary contract

`mur-agent-launcher` (shipped inside `MUR Agent Host.app/Contents/MacOS/`, copied per stub):

1. Reads `Contents/Resources/agent.txt` → agent slug.
2. Reads `~/.mur/host_path` → absolute path to current Host binary, written by Host on every start. If absent or stale (Host version field doesn't match the version recorded in stub's `agent.txt`), triggers Hub's stub-regeneration flow before continuing.
3. Reads `Contents/Resources/host_version.txt` (written at stub-generation time) → expected host version. On mismatch with `host_path`'s recorded version, regenerate-stub flow is invoked.
4. If invoked via URL: forwards raw URL.
5. If invoked via NSServices: writes pasteboard contents to `/tmp/mur-share-<uuid>` (mode 0600).
6. `execv`s `<host_path>/Contents/MacOS/mur-host` with args `--agent <slug> [--url <U> | --share-from-file <F>]`. **NOT `open -b run.mur.host`** — direct exec avoids LaunchServices indirection, ensures argv[0] preservation, and prevents stale-bundle collision when multiple Host installs exist.
7. Exits. Lifetime measured in milliseconds.

> **Why direct exec and not `open -b run.mur.host`:** the previous draft used `open -b`, which bounces through LaunchServices and could resolve to any app claiming the bundle id (including stale copies). Chrome's app_shim uses direct framework load for exactly this reason. Our launcher uses direct binary exec, which is the simpler equivalent — no framework, just a hard-linked path read from a host-written sidecar file. See [Chromium Mac App Mode design doc](https://www.chromium.org/developers/design-documents/appmode-mac/) and [chrome/app_shim/](https://chromium.googlesource.com/chromium/src/+/lkgr/chrome/app_shim/).

**Hard requirements:**

- `MUR Agent Host.app` MUST be installed in `/Applications` (not `~/Applications`) for reliable LaunchServices URL scheme dispatch. Stubs live in `~/Applications/MUR-Agent-<Slug>.app/` (different location intentional — stubs need user-write access, Host benefits from system-wide registration).
- Host MUST write absolute path + version to `~/.mur/host_path` on every startup, atomically (write to `.tmp` + rename).
- Launcher binary size budget: < 100 KB statically linked. No Tauri, no async runtime, no `@rpath` to frameworks (avoids the 2022 Chromium PWA designated-requirement breakage).
- Launcher MUST be ad-hoc signed (`codesign -s - --force --timestamp=none`) after copying. Do **NOT** apply `--options=runtime` (hardened runtime) to the launcher.
- Stub-generation MUST validate post-sign: `codesign --verify --deep --strict <stub>` and `spctl --assess --type execute --verbose=4 <stub>` (see §12.4).
- After stub `.app` written, run in order: `lsregister -f <stub>` → `LSSetDefaultHandlerForURLScheme(muragent-<slug>, run.mur.agent.<slug>)` → `pbs -update`.

### 5.3 Per-agent single-instance IPC

Tauri's `tauri-plugin-single-instance` enforces one Host UI process — but our model is "one Host UI process, multiple agents." Per-agent activation needs an additional IPC layer keyed on slug, because:

- macOS users may install the same Host twice (e.g., during dev). Stub launcher must always reach the *right* Host instance for its agent.
- Wayland Linux has **no external focus API**. The only way to raise an existing agent window is for the running Host to listen on a per-agent channel and self-surface.
- Re-launching a stub for an already-running agent must forward URL/share args to the existing window, not spawn a duplicate.

Protocol:

```
Channel: ~/.mur/agents/<slug>/ipc.sock (Unix domain socket, mode 0600)
         \\.\pipe\mur-agent-<slug>        (Windows named pipe, owner-only DACL)

Host (when --agent <slug> is invoked):
  1. Try acquire lock by binding the socket / opening the pipe with exclusive access.
  2. If bind succeeds:
       a. Open or focus the agent's window.
       b. Listen for incoming activation messages; on receive, raise + apply.
  3. If bind fails (another Host owns the channel):
       a. Connect as client.
       b. Send activation payload (URL / share-file / bare focus request).
       c. Exit 0.

Activation payload (CBOR-encoded for simplicity):
  { "kind": "url"|"share"|"focus",
    "url":  "<optional>",
    "share_file": "<optional path>",
    "ts":   <unix>,
    "nonce": <bytes> }
```

Permission posture: socket mode 0600 / pipe owner-only DACL prevents cross-user access. Payloads are treated as untrusted; URL payloads are validated against the manifest's signed `url_scheme` before any side effect.

### 5.4 Stub self-update on Host upgrade

Stubs go stale when Host updates: launcher binary may have new features, `host_path` may have changed, signing identity may have rotated. Without a regeneration flow, stubs from before the upgrade are silent dead weight (the 2022 Chromium PWA failure mode at scale).

On every Host startup:
1. Read all stubs from the platform's stub directory.
2. For each stub, check the version recorded in `Contents/Resources/host_version.txt` (macOS) / equivalent registry value (Windows) / `X-Mur-Host-Version=` key in `.desktop` (Linux).
3. If any stub's version is older than current Host: enqueue regeneration. Run in background after UI ready (low priority, ~50 ms per stub).
4. Regeneration is idempotent — wipes the stub, recreates from current Host's templates, re-signs, re-asserts URL scheme handler. User sees no UI; if a stub is in active use it's regenerated next time it's not in use.

## 6. `.muragent` v2 File Format

`.muragent` is a `tar.gz` archive. v2 is the only supported version — the prior `.murpkg` v1 format (see `mur-agent-runtime/src/export/pkg.rs`) is superseded with no migration path per §1.1.

### 6.1 Archive layout

```
coach.muragent  (tar.gz)
├── manifest.yaml                      # schema, identity, surface-specific blocks (§6.2)
├── manifest.signed.json               # JCS canonical projection of manifest.yaml (§6.3)
├── signatures.json                    # DSSE envelope (§6.3)
├── profile.yaml                       # sanitized AgentProfile (private key stripped)
├── icon/
│   ├── icon.icns                      # Hub: macOS
│   ├── icon.ico                       # Hub: Windows
│   └── icon-512.png                   # Hub: Linux + fallback / Commander: chat avatar
├── voice/                             # optional; iff hub.voice.enabled
│   └── voice.yaml
├── assets/
│   ├── (author-supplied static assets — avatars, presets, ...)
│   └── commander/                     # optional; iff commander: block present
│       ├── workflows/*.yaml
│       └── programs/*.md
```

`assets/commander/` is namespaced because Commander's workflow / program files are first-class artifacts that need their own subtree. Hub's surface-specific assets live directly under `assets/` (no `hub/` subdir) because there are no nested file collections for Hub today — if that changes, a future `assets/hub/` namespace can be added without schema changes.

### 6.2 `manifest.yaml` schema

```yaml
schema: mur-agent/2                    # canonical version; no v1 compat path
exported_at: 2026-05-20T12:34:56Z
exporter:
  mur_version: 2.13.0                  # could be "mur" or "murc" — see exporter.tool
  tool: mur                            # "mur" (Hub-side) or "murc" (Commander-side)
  min_hub_version: 2.13.0              # ignored if hub: block absent
  min_commander_version: 0.10.0        # ignored if commander: block absent

agent:
  slug: coach                          # kebab-case; OS identity uses this (§5.0)
  display_name: Coach
  bundle_id: run.mur.agent.coach       # MUST equal "run.mur.agent.<slug>" — see §5
  url_scheme: muragent-coach           # Hub surface only; Commander ignores
  original_uuid: 8f3a...               # AgentProfile.id (for update detection)

required_surfaces:                     # at minimum which surface MUST be installed
  - hub                                # OR commander OR both
                                       # if both: surface absent on recipient = warning, not refuse

optional_capabilities:                 # surfaces feature-flag against this list
  - voice                              # Hub only
  - idle_triggers                      # Hub (C6)
  - slack_bridge                       # Hub (C7) — note: distinct from commander.chat_platforms
  - workflow_engine                    # Commander only
  - jira_integration                   # Commander only
  - sub_agents                         # Commander only
  # recipient surfaces silently skip unknown capabilities; UI surfaces "missing capability" hints

# ─── Shared (consumed by both surfaces) ────────────────────────────────────
profile: { ... }                       # sanitized AgentProfile reference;
                                       # actual content in profile.yaml (separate file)

mcp_servers:                           # informational at manifest level; full list in profile.yaml
  - name: context7
    command_basename: npx

icon:
  formats: [icns, ico, png]
  hash:
    icns: sha256:<hex>
    ico:  sha256:<hex>
    png:  sha256:<hex>

sanitized:                             # transparency: what export stripped
  removed_fields:
    - identity.private_key
    - identity.api_keys

# ─── Hub-specific (Commander ignores entire block) ─────────────────────────
hub:                                   # OPTIONAL — present iff Hub features used
  appearance:
    style_preset: chibi                # see hub-companion design
    behavior_preset: normal
  voice:
    enabled: true                      # ⇒ "voice" must be in optional_capabilities
  pet:
    enabled: true
  url_scheme_overrides: []             # advanced; per-surface URL routing tweaks

# ─── Commander-specific (Hub ignores entire block) ─────────────────────────
commander:                             # OPTIONAL — present iff Commander features used
  chat_platforms:
    - slack
    - telegram
  workflows:                           # list of workflow files in assets/commander/workflows/
    - name: morning-standup
      file: assets/commander/workflows/morning-standup.yaml
      schedule: "0 9 * * 1-5"
  programs:                            # Program.md strategy layer
    - file: assets/commander/programs/research.md
  jira:
    base_url: https://example.atlassian.net
    secret: env:JIRA_TOKEN             # SecretRef — uses existing codec prefixes: env:|keychain:|file:|cmd:
                                       # the recipient substitutes their own JIRA_TOKEN env var
  sub_agents:
    max_concurrent: 5
  schedule_defaults:
    timezone: Asia/Taipei
```

The DSSE envelope (§6.3) signs the **complete** manifest including all surface-specific blocks. Each surface validates the whole signature but reads only its own block plus the shared fields. A surface receiving a manifest with an unknown block (e.g., a future `mobile:` block) MUST ignore the unknown block, not reject the file — this is the forward-compatibility rule.

### 6.3 Signing format — DSSE envelope over in-toto Statement

Industry consensus (in-toto, SLSA, Sigstore, TUF) is to sign a **DSSE envelope** containing an **in-toto v1 Statement**. This avoids canonical-YAML fragility (no canonical YAML standard exists), provides multi-signature support natively (the V2 mur badge fits as a second envelope signature), and remains compatible with a future Sigstore migration.

**Tarball layout (additions):**

```
coach.muragent (tar.gz)
├── manifest.yaml             # human-readable; NOT the signed bytes
├── manifest.signed.json      # canonical JSON projection of manifest.yaml (RFC 8785 JCS)
├── signatures.json           # DSSE envelope (author + optional mur signature)
├── profile.yaml              # sanitized AgentProfile
├── icon/...
├── voice/...
└── assets/...
```

**`manifest.signed.json` derivation rules (author MUST follow at export time):**

1. Parse `manifest.yaml`.
2. Reject if any of: YAML anchors, aliases, merge keys (`<<:`), duplicate keys, non-string keys, native (`!!timestamp` / `!!date`) timestamps. Reject silently-quoted variants of the Norway problem (`no`, `false` etc. as bare values are coerced — require explicit strings).
3. Reject any file path in the tarball that contains a NUL byte, control character, backslash, `..` component, or absolute prefix.
4. Normalise all string values to Unicode NFC (paths AND all other string fields — prevents cross-system canonicalization divergence for display names, descriptions, etc.).
5. Emit RFC 8785 canonical JSON (lex-sorted keys, no insignificant whitespace, JSON-only number serialization).

**JCS implementation note:** The existing `mur-common::canonical` module explicitly does NOT implement RFC 8785 (it uses a simpler canonicalization sufficient for identity rotation signatures). The `.muragent` JCS canonicalizer MUST be a separate implementation (`mur-common::jcs`) that fully conforms to RFC 8785, including its number-serialization rules (no scientific notation, no trailing zeros, `1e10` → `10000000000`). Do NOT reuse the existing `canonical` module for `.muragent` — the two canonical forms serve different contracts and must not be conflated.

**In-toto v1 Statement built at export time:**

```json
{
  "_type": "https://in-toto.io/Statement/v1",
  "subject": [
    { "name": "profile.yaml",      "digest": { "sha256": "<hex>" } },
    { "name": "icon/icon.icns",    "digest": { "sha256": "<hex>" } },
    { "name": "icon/icon.ico",     "digest": { "sha256": "<hex>" } },
    { "name": "icon/icon-512.png", "digest": { "sha256": "<hex>" } },
    { "name": "voice/voice.yaml",  "digest": { "sha256": "<hex>" } },
    { "name": "assets/...",        "digest": { "sha256": "<hex>" } }
  ],
  "predicateType": "https://mur.run/agent-manifest/v1",
  "predicate": {
    "manifest_sha256": "<sha256 hex of manifest.signed.json bytes>"
  }
}
```

`subject` lists every tarball file **except** `manifest.yaml`, `signatures.json`, and `manifest.signed.json` itself, sorted lex by NFC-normalised path. Each subject is a structured JSON object (not concatenated bytes) — this defeats the path-collision second-preimage attack class that the original `path || 0x00 || hash` construction would have been vulnerable to.

> **Why `manifest_sha256` and not parsed content:** embedding the parsed JSON object as the predicate creates a JSON parse-re-serialize round-trip risk: the author's JCS serializer may format numbers differently from the verifier's (`1e10` vs `10000000000`), causing byte-for-byte comparison to fail. A SHA-256 hash eliminates this entire class of bugs — the verifier computes SHA-256 of the raw `manifest.signed.json` bytes and compares the hex digest. No re-serialization, no canonicalization divergence.

**DSSE PAE (Pre-Authentication Encoding):**

```
PAE = "DSSEv1 " || len(payloadType) || " " || payloadType || " " || len(payload) || " " || payload
payloadType = "application/vnd.in-toto+json"
payload     = utf8(canonical_json(statement))
```

Signature = `Ed25519(PAE)`. PAE binds the `payloadType` into the signed bytes, so a verifier can never be tricked into interpreting an in-toto Statement as something else (the alg-confusion class of bugs that killed JWS-EdDSA in earlier ecosystems).

**`len()` means byte length, not character count.** All three `len()` calls in the PAE formula count UTF-8 bytes. `payloadType` is the fixed ASCII string `"application/vnd.in-toto+json"` (25 bytes) — this is unambiguous, but implementations MUST use byte-length primitives (Rust: `.len()`, Go: `len()`, Python: `len(x.encode("utf-8"))`), never character-count functions.

**`signatures.json` (DSSE envelope shape):**

```json
{
  "payloadType": "application/vnd.in-toto+json",
  "payload":     "<base64 of canonical_json(statement)>",
  "signatures": [
    {
      "keyid":     "ed25519-<first-8-hex-of-sha256(pubkey)>",
      "publicKey": "<base64 32 bytes>",
      "sig":       "<base64 64 bytes>"
    }
    /* V2: a second entry appears here when mur issues a verified badge —
       same payload, signed by the mur root key */
  ]
}
```

This **collapses our previously-separate `signatures.author` and `signatures.mur` blocks into one DSSE envelope** with one or two signature entries. V2 issuing a mur badge means appending a second entry to `signatures[]` against the same payload — no payload re-upload, no re-canonicalisation, no new file format.

**Rust implementation:** `ed25519-dalek` v2 (constant-time, zeroize-on-drop, `verify_strict`) + a thin DSSE helper (~80 lines). Reference implementations: [secure-systems-lab/dsse](https://github.com/secure-systems-lab/dsse), `sigstore-rs`.

### 6.4 Validation rules (v1 Hub)

On import, Hub validates in order. **Every step's failure is fatal**: there is NO "continue anyway" path for any signature or integrity error (Cydia's failure mode — see §7.5).

1. **Tarball integrity** — gz CRC, tar entries readable, no symlinks, no entry escapes archive root.
2. **No executable content** (hard fail):
   - **Tar mode bits:** Reject any regular file entry with execute mode bits (directories may have execute bits — directory traversal is not execution).
   - **Extension block (case-insensitive):** Reject any entry whose lowercased path ends in `.so`, `.so.<version>` (versioned shared libraries), `.dylib`, `.dll`, `.exe`, `.dmg`, `.pkg`, `.msi`, `.AppImage`, `.elf`, `.wasm`, `.bin`, `.sys`, `.ko` (kernel module), `.kext`, `.app` (nested bundle), `.sh`, `.bash`, `.zsh`, `.fish`, `.py`, `.rb`, `.pl`, `.php`, `.lua`, `.js` (standalone script files — assets inside `assets/commander/` may contain `.js`/`.ts` as data for workflow definitions, not as executable code; those are exempt because they live under a known data namespace and are never directly invoked by the runtime).
   - **MCP command restrictions:**
     - Reject any `mcp_servers[].command` that is an absolute path or contains `/` or `\`.
     - Reject any `mcp_servers[].command` whose basename appears in the **interpreter deny-list**: `sh`, `bash`, `zsh`, `dash`, `fish`, `python`, `python3`, `ruby`, `perl`, `php`, `node`, `deno`, `bun`, `lua`, `luajit`, `awk`, `Rscript`, `groovy`, `kotlin`, `scala`, `jq`, `execline`, `rc`. Additionally reject any command containing `-e`, `--eval`, `-c`, `--command`, `-r`, `--require`, `-exec`, `--exec` as standalone arguments (these flags enable inline code execution in interpreters).
     - Reject any command containing shell metacharacters: `|`, `;`, `&`, `$()`, `` ` ``, `$((`, `() {`, `&&`, `||`, `>`, `<`, `!` (excluding `!` in negated flags like `--no-foo` which is safe).
     - Reject any `mcp_servers[].args` entry that contains the same metacharacters.
     - Reject commands that chain package-manager installs: any command containing `install` or `add` together with `&&`, `;`, `|`, or newline.
   - **Permit-list for MCP commands (defense-in-depth):** beyond the deny-list, the validator SHOULD check that the command basename appears in a curated permit-list. The v1 permit-list includes: `npx`, `uvx`, `docker`, `podman`, `git`, `gh`, `npm`, `yarn`, `pnpm`, `cargo` (restricted to subcommands `check`/`build`/`test`/`run`/`clippy`/`fmt` — NOT `install`), common Unix tools (`curl`, `wget`, `jq`, `rg`, `fd`, `sd`, `bat`, `delta`, `ghostscript`, `imagemagick`, `ffmpeg`, `sqlite3`, `psql`, `mysql`, `redis-cli`). Commands outside the permit-list trigger a **warning** (not hard fail) so new tools can be added organically. V2 may hard-fail on unknown commands after the permit-list stabilizes.
   - **Runtime sandbox (defense-in-depth):** regardless of static validation, every MCP server process MUST be spawned inside an OS-level sandbox where available: macOS `sandbox-exec` with a minimal profile (network-out-only, no file writes outside `$TMPDIR`), Linux Landlock LSM (ruleset covering the agent's home directory only), Windows AppContainer (network-only capability). This catches interpreter-based execution vectors that the static deny-list cannot cover (e.g., `npx evil-pkg` downloading and executing arbitrary code at runtime).
   - **Path safety:** Reject any path containing NUL bytes, control characters, backslashes, `..`, or absolute prefixes (per §6.3 derivation rules). Apply case-insensitive comparison for extension matching against a lowercased version of the path.
3. **Schema version** — `schema: mur-agent/2` required exactly; any other value (including legacy `mur-agent-package/1`) is rejected with no compat path.
4. **Version compatibility** — `min_host_version` ≤ current Hub version ≤ `max_host_version`.
5. **`manifest.signed.json` matches `manifest.yaml`** — re-derive canonical JSON from `manifest.yaml`, compare byte-for-byte to embedded `manifest.signed.json`. Mismatch = fatal.
6. **DSSE envelope structure** — well-formed JSON, `payloadType == "application/vnd.in-toto+json"`, ≥1 signature entry, all signature entries decode.
7. **Statement structure** — payload decodes to in-toto Statement v1 shape, `predicateType == "https://mur.run/agent-manifest/v1"`, `predicate.manifest_sha256` equals SHA-256 of the raw `manifest.signed.json` bytes extracted from the tarball.
8. **Author signature** — first `signatures[]` entry MUST verify (`Ed25519.verify_strict(PAE, signature, publicKey)`). Use `ed25519-dalek`'s strict verifier (rejects non-canonical encodings and small-order points).
9. **Subject hashes** — every file listed in `statement.subject` exists in the tarball with matching sha256; every tarball file (excluding `manifest.yaml` / `signatures.json` / `manifest.signed.json`) is listed in `subject`.
10. **Mur signature** (v1: ignored; V2: verify against embedded root pubkey set — §7.4). Failure is fatal in V2 only if the user has opted in to "mur-verified-only" filtering.
11. **Revocation check** (v1: skip; V2: consult cached `revocations.json` — §7.4).

## 7. Trust Model

### 7.1 V1 scope

- **Author signature is mandatory** for `.muragent` files. There is no unsigned path. Per §1.1 there is no `.murpkg` v1 to support either.
- **Mur signature slot is reserved but unverified.** V1 Hub and Commander both ignore the second DSSE signature entry; verification ships in V2.
- **Local trust store at `~/.mur/trust/trust.yaml`** records every author pubkey either surface has ever imported. **Shared across surfaces** — Hub and Commander read and write the same file (atomic write via tmp+rename; both surfaces tolerate concurrent reads):

```yaml
agents:
  - public_key: <ed25519-base64>
    display_name_seen: Coach
    first_seen: 2026-05-20T12:00:00Z
    last_seen: 2026-05-20T12:00:00Z
    last_seen_surface: hub             # which surface last imported (informational)
    trust_level: known                 # known | pending | rejected | superseded
    fingerprint: sha256:abcd...        # short form for UI display
    word_list: "tango victor whiskey alpha"   # 4-word fingerprint for human verify (Signal-style)
    rotated_from: null                 # set by §7.1.1 rotation manifest
    superseded_at: null
    last_rotation_at: null             # timestamp of most recent rotation; used for replay prevention (§7.1.1)
```

Path layout:

```
~/.mur/trust/
├── trust.yaml                 # local trust store (shared)
├── rotations/                 # received rotation manifests, keyed by new_pubkey fingerprint
│   └── <fingerprint>.rotation
└── revocations.json           # V2 only; consumer scaffolding shipped in v1
```

Concurrent-write posture: Hub and Commander both write `trust.yaml` rarely (on import) and read it on every signature verification. Use file lock (`fcntl::flock` on macOS/Linux, `LockFileEx` on Windows) during write; readers retry on transient lock failures. Race window is small (single import operations); no daemon-coordinator needed.

**Implementation note — path and format reconciliation:** The existing Commander trust store (`crates/engine/src/trust/store.rs` in the `mur-commander` repo) writes to `~/.mur/trust.json` (a flat JSON file), not `~/.mur/trust/trust.yaml`. Before the shared trust store ships, the two surfaces MUST reconcile on the canonical path and format defined here. The migration path: on first access, `mur-common::trust` checks for the legacy `~/.mur/trust.json`; if present and no `~/.mur/trust/trust.yaml` exists, migrate entries to the new format and path atomically, then remove the legacy file. Both surfaces call the same `mur-common::trust` module so the migration runs exactly once.

- First import of a previously-unseen pubkey → `trust_level: pending`. UI surfaces the §7.2 prompt (Hub: import dialog; Commander: chat confirmation message).
- Subsequent imports of the same pubkey → accepted, but a **lightweight permission acknowledgement** is always shown (one line summarizing new permissions: MCP servers, network mode, voice, idle triggers). This requires a single click to dismiss — not a full re-consent dialog, but a deliberate action that prevents capability-creep. An author who builds trust with benign agents and later publishes one with expanded capabilities cannot slip new permissions past a user who is clicking through on muscle memory.
- Display-name change on same pubkey → forces full §7.2 re-confirm (adopted from VS Code Marketplace's verified-publisher revocation trigger).
- **Known author, key changed, no rotation manifest = HARD REFUSE.** This is the actual MITM detection surface. The user must explicitly remove the old entry from `trust.yaml` before the new key can be imported. This is the SSH known_hosts pattern — and unlike WhatsApp's non-blocking "identity changed" toast, the warning is blocking, because at the trust frontier non-blocking warnings train users to dismiss them.
- For legitimate key rotation, see §7.1.1.

#### 7.1.1 Key rotation manifest

A naive "new key = new author" rule fails catastrophically in two real scenarios: (a) author's signing-laptop is lost / signing key is rotated for hygiene, (b) the 2008-Debian-OpenSSL class of forced rotations. Without a rotation path, every legitimate rotation creates a "first-time author" UX cliff that trains users to dismiss the warning — exactly the failure mode that erodes SSH known_hosts trust.

Adopt Tor's pattern: a separate rotation manifest, signed by both old AND new keys, exported as a standalone artifact.

```yaml
# rotation manifest (separate file: <author>.rotation, signed by both keys)
old_pubkey: <base64 32 bytes>
new_pubkey: <base64 32 bytes>
issued_at: 2026-05-20T12:00:00Z
sig_old: <Ed25519(old_pubkey || new_pubkey || issued_at) by old_key>
sig_new: <Ed25519(old_pubkey || new_pubkey || issued_at) by new_key>
```

Hub flow on encountering a `.muragent` signed by an unknown pubkey when a known pubkey exists for that display name:
1. Look for a `rotation` artifact (file in Hub's known location, or fetched from author registry in V2).
2. If found and `old_pubkey ∈ trust.yaml` as `known` (any trust_level except `rejected`), verify both signatures.
3. **Replay prevention:** verify `issued_at > trust_entry.last_rotation_at` (if `last_rotation_at` is set). Reject rotation manifests whose `issued_at` is not strictly newer than the last recorded rotation. This prevents an attacker from replaying a historical rotation manifest (e.g., key_A→key_B) to resurrect a superseded key after the author has already rotated to key_C.
4. **Trust-level gating:** if the old pubkey's `trust_level` is `superseded`, the rotation is still accepted (it's a chain rotation: A→B→C where B is already superseded), but `last_rotation_at` on the *original* entry (key_A) must still be older than `issued_at`.
5. On verify success: append `new_pubkey` to `trust.yaml` with `rotated_from: <old>` and `last_rotation_at: <issued_at>`. Mark old entry `trust_level: superseded, superseded_at: <ts>`, and set `last_rotation_at: <issued_at>` on the old entry too. Old artifacts still verify against the superseded key (flagged as "superseded source" in UI); new artifacts must use the new key.
6. On verify failure (or no rotation artifact): hard refuse per §7.1.

### 7.2 First-time-author prompt — UI design

Research finding: SmartScreen's wall-of-text dialog trained users to "More info → Run anyway". Gatekeeper's right-click-Open friction works because the override is *not the default button*. Cydia died partly because hash-mismatch warnings were treated as advisory.

V1 prompt design rules:

1. **Frame as observation, not warning.** Text: *"First time you've imported anything from this author."* No "WARNING" prefix, no scariness.
2. **Show display name prominently; fingerprint as confirmable detail.** 8-hex-character short fingerprint (first 8 hex of SHA-256 of pubkey) **plus a 4-word fingerprint** ("tango victor whiskey alpha") for human side-channel verification. The 4-word fingerprint uses the **EFF long word list** (7776 words, `https://eff.org/files/2016/09/08/eff_long_wordlist.txt`), encoding 51.6 bits of the SHA-256(pubkey) truncated to 52 bits then split into four 13-bit indices. This provides ~52 bits of entropy — sufficient for human comparison against a known-trusted fingerprint shared out-of-band, while being short enough to read aloud in a phone call. The 8-hex-char fingerprint (32 bits) is for quick visual scanning only and MUST NOT be relied upon as the sole identity check.
3. **No "always trust this author" checkbox.** Trust accrues from successful imports. A checkbox is a click-through training device.
4. **Two buttons: Cancel (left, secondary), Import (right, primary).** Primary action is the user's clearly-intended one — install the agent they chose to import. Asymmetric override (à la SmartScreen "Run anyway" being the prominent button) is anti-pattern in the *opposite* direction; here we don't have a safe default to push them toward, so the cleanest path is symmetric buttons with no nag checkboxes. **Import button activates after a 5-second delay** on the first-time prompt — this prevents muscle-memory click-through (the user must pause and read the permissions list) without adding a separate "I have read" checkbox. The delay applies only to first-time-author prompts; known-author imports have no delay.
5. **Surface declared permissions in the same dialog.** Signature proves *who*; permissions tell user *what*. Show: MCP servers it will spawn, network outbound mode (per `entitlements.network.outbound.mode`), idle/scheduled triggers, voice/microphone usage. These come from the verified `manifest.signed.json`.
6. **Keep prompt body under 4 lines** plus the permissions list.

Badge text in the trust-status row (V1 surface):

| Source / state | Badge text |
|---|---|
| Valid author signature, first-seen pubkey | "First time you've imported from this author" |
| Valid author signature, known pubkey | (no badge — trust is the silent default for known authors) |
| Valid author signature, key changed for known author WITH rotation manifest | "Author rotated their signing key on `<date>`" |
| Valid author signature, key changed for known author WITHOUT rotation manifest | (import refused; surfaced as error dialog, not badge) |
| Signature invalid / DSSE structure broken | (import refused; surfaced as error dialog) |
| V2: mur-verified author signature present | "✓ mur-verified author" (green) |
| V2: mur-verified, then revoked | "Verification revoked — proceed with caution" (amber) |

The same DOM element renders V2's mur badge — V1 ships the conditional rendering with the mur branch dark.

### 7.3 Mur root-of-trust public key (V1 prep)

Hub binary embeds a hardcoded constant for the mur root signing key set:

```rust
// mur-gui-core/src/trust/root.rs
pub const MUR_ROOT_PUBKEY_V1: [u8; 32] = [
    // populated at v2.13.0 release time; rotation handled via §7.1.1.
    0x00; 32,  // placeholder until release prep
];

pub const MUR_ROOT_PUBKEYS: &[&[u8; 32]] = &[&MUR_ROOT_PUBKEY_V1];
```

V1 does not call any verification function on these constants — they're embedded to lock the key material into the supply chain before V2 needs them. The key itself is generated (offline, in an air-gapped or HSM-bound ceremony) and committed in the release-prep PR for the first Hub release that ships this spec. Subsequent Hub releases MAY ship additional root keys via the same constant; verifiers accept signatures from any pubkey in the set.

### 7.4 V2 roadmap (informational, not in scope for this spec)

For reference so V1 decisions stay forward-compatible:

1. Pro user uploads `.muragent` to `verify.mur.run` with manifest + author pubkey.
2. Automated checks: manifest shape, MCP endpoint denylist, prompt keyword scan, payload anomaly detection (the in-toto Statement subjects make file-set inspection trivial).
3. Light human review (5-min budget at launch volumes).
4. On approval, mur signs the **same DSSE PAE** the author signed (using a mur root key from §7.3) and **appends** the signature to `signatures.json` — no new envelope, no new file format.
5. Hub displays "✓ mur-verified author" badge.
6. Display-name change after badge issue revokes the badge automatically (VS Code Marketplace pattern). Encoded in `revocations.json` as a revoke-by-manifest-hash entry.

**Sybil resistance:** the Pro-tier payment instrument (Stripe-bound card) is a stronger sybil tax than free email/2FA (which npm uses and which Shai-Hulud-class attacks demonstrably defeated). Not perfect (prepaid cards exist) but strictly better than zero-cost identity.

**Critical UX rule, borrowed from Flathub and VS Code:** *the badge means "we know who to call," not "this is safe."* Cydia died because curation was sold as safety. Mur badges are identity attestations, not behavior attestations.

#### 7.4.1 `revocations.json` — signed revocation channel

Modeled on TUF's `timestamp.json` role, scaled down. Not in v1 scope; defined here so v1 decisions stay forward-compatible.

```json
{
  "version": 1,
  "this_update": "2026-08-01T00:00:00Z",
  "next_update": "2026-08-02T00:00:00Z",     // 24-hour cadence; Hub refreshes daily
  "expires_at":  "2026-08-08T00:00:00Z",      // 7-day fail-closed staleness
  "crl_number":  142,                          // monotonic; rollback detection
  "revoked": [
    {
      "kind": "package",                       // revoke a specific .muragent
      "manifest_hash": "sha256:<hex>",         // hash of manifest.signed.json
      "reason": "verified-badge-revoked",
      "revoked_at": "2026-08-01T12:00:00Z"
    },
    {
      "kind": "author",                        // revoke an author entirely
      "pubkey": "ed25519:<base64>",
      "reason": "credentials-compromised",
      "revoked_at": "2026-07-30T18:00:00Z"
    }
  ]
}
```

Signed by an offline mur root key from §7.3 (DSSE envelope, same primitive). Hub fetches via the existing Hub update channel on a daily timer. If `expires_at` is in the past, Hub refuses to operate (no stale-trust-during-network-block attack); the user sees a "Trust list expired — connect to refresh" dialog.

**Rollback and clock-rollback defense:**

- **`crl_number` monotonicity:** Hub MUST reject any `revocations.json` whose `crl_number` is ≤ the last known `crl_number`, regardless of its `expires_at`. This provides protection against clock-rollback attacks: even if the system clock is rolled back to a time when an old revocation list was still valid, the monotonic counter prevents re-acceptance.
- **First-fetch bootstrapping:** on first fetch (no prior `crl_number`), accept any validly-signed `revocations.json` with a future `expires_at`. The integrity guarantee comes from the mur root key signature on the file, not from the counter.
- **7-day fail-closed tradeoff:** the 7-day expiration balances security (shorter window for a compromised key to operate while revocation is blocked) against availability (network outages, users traveling offline). The parameter is configurable by the revocation issuer (`expires_at` is set at signing time). V2 may reduce this to 72 hours based on operational experience.

**Revoke granularity:** prefer `kind: package` (specific manifest hash) over `kind: author` (entire pubkey). Author-level revocation should be reserved for true credential compromise — has the optics of an Apple-style kill-switch and should be used sparingly.

**Anti-whack-a-mole extension:** a `kind: package` revocation only covers a specific `manifest_hash`. A malicious author can re-publish a trivially-modified version (different hash, same malicious content) to bypass the revocation. When the content itself is problematic (not just a specific signed instance), use `kind: author` to revoke the entire pubkey. V2 may add a `kind: agent_slug` revocation that covers all versions of a named agent from a specific author, bridging the granularity gap between single-manifest and entire-author.

### 7.5 Critical rule: signature failure is fatal, never advisory

Across the entire validation pipeline (§6.4), **no signature, integrity, or revocation failure may be overridable by the user**. There is no "Continue anyway" button. There is no preference toggle to disable signature checking. There is no `--insecure` flag.

This is non-negotiable because the *one* lesson Cydia teaches is that the moment users learn they can click past signature errors, the entire signing infrastructure stops providing any security benefit. The only signature-related click-through path is the §7.2 first-time-author prompt — and that is for a *valid* signature from an *unknown* author, not for an invalid signature.

If a user genuinely needs to import a corrupted `.muragent`, the correct answer is "ask the author to re-export it." There is no graceful degradation for broken signatures.

## 8. CLI Surface

### 8.1 Author commands

```
mur agent export <name>                            # default behavior, see timeline
mur agent export <name> --format=muragent          # explicit data-file output
mur agent export <name> --standalone               # legacy 13-phase per-agent .app pipeline
mur agent export <name> --format=muragent --out PATH
mur agent export <name> --sign-with PATH           # custom signing key (default: agent's identity key)
```

### 8.2 No deprecation timeline

Per §1.1 there are no production users of the previous export path, so no multi-release deprecation is needed. The first release that ships this spec is `v2.13.x` (or whichever follows on main):

- Default of `mur agent export <name>` switches **directly** to `.muragent` output.
- `--gui` flag is **removed** entirely (or accepted as a hard error explaining the rename to `--standalone`).
- `--standalone` flag accepts the legacy 13-phase per-agent `.app` pipeline, gated behind `MUR_APPLE_DEVELOPER_ID` env var as before.
- `--format=muragent` is accepted as an explicit alias of the default (forward-compatible if we later add other output formats).

There is no CI lint or warning phase — anyone scripting `--gui` will get an immediate hard error pointing at `--standalone`. This is the right friction given no real users.

A parallel `murc agent export` on the Commander side ships the **same** `.muragent` output by re-using `mur-common::muragent::Writer` (§16.6). Commander's CLI surface is otherwise out of scope for this spec.

### 8.3 Recipient commands (CLI-side; Hub provides the GUI equivalents)

```
mur agent install <path-to-.muragent>              # imports without launching Hub
mur agent install <path-to-.muragent> --auto-start # also wires launchd / systemd / Run
mur agent uninstall <name>                         # reverses install
mur agent list                                     # lists installed agents
mur agent inspect <path-to-.muragent>              # prints manifest + verifies signature, no install
```

## 9. Host Distribution and CLI Co-Installation

Per **B3** decision:

### 9.1 Two parallel install paths

```
# Path A: Homebrew (independent components, Homebrew convention)
brew install mur                       # CLI only
brew install --cask mur-hub            # Host.app only
brew install mur mur-hub               # both (documented meta-command)

# Path B: Official download (Host with embedded CLI)
# User downloads MUR Agent Host.dmg from mur.run/get
# Drags to /Applications
# Opens Host → Preferences → Advanced → [Install Command Line Tool]
#   ↓
# Host copies its embedded mur binary to /usr/local/bin/mur (sudo prompt)
```

### 9.2 Embedded CLI inside Host bundle

```
MUR Agent Host.app/
├── Contents/
│   ├── MacOS/
│   │   ├── MUR Agent Host           # Tauri binary
│   │   ├── mur-agent-runtime        # universal sidecar
│   │   └── mur-agent-launcher       # stub launcher template
│   └── Resources/
│       └── cli/
│           └── mur                   # CLI binary, same version as Host
```

Hub Preferences → Advanced exposes:
- **Install Command Line Tool** button: `osascript -e 'do shell script "ln -sf ... /usr/local/bin/mur" with administrator privileges'`. Idempotent; if `/usr/local/bin/mur` already exists and points elsewhere, prompts to overwrite.
- **Uninstall Command Line Tool** button: removes the symlink (does not touch Homebrew-installed CLI).

On Host update, the embedded CLI is updated automatically; if the symlink is in place, the next `mur` invocation hits the new version. If the user has both Homebrew CLI and Host-installed CLI, PATH ordering decides which wins — Hub's installer warns when it detects a Homebrew install and offers to skip.

### 9.3 Windows / Linux equivalents

- **Windows:** Host installer (MSIX or NSIS) optionally adds `%LOCALAPPDATA%\Programs\MuR\bin` to PATH; embeds `mur.exe` in `%LOCALAPPDATA%\Programs\MuR\bin\mur.exe`. No symlink dance.
- **Linux:** Host AppImage doesn't write outside its bundle (AppImage convention). Instead, `mur agent doctor` instructs the user to `ln -s "$(appimage path)/mur" ~/.local/bin/mur` or install the standalone CLI tarball.

### 9.4 Windows code signing — Microsoft Trusted Signing

**EV certificates no longer bypass SmartScreen** as of March 2024 ([Microsoft Learn](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)). Both OV and EV must build reputation organically, and reputation resets per binary hash. Microsoft's current recommendation is **Trusted Signing** ($9.99/mo, cloud-managed certs), which reportedly accumulates SmartScreen reputation faster than third-party OV at our scale.

Adopt Trusted Signing for `MUR Agent Host.exe` (and the embedded `mur.exe` CLI) from the first release that ships the Host + data model. Keep a stable signing identity across releases — every release with a new signer resets SmartScreen reputation to zero.

Per-agent `.lnk` shortcuts do NOT need signing (SmartScreen is target-binary-keyed, not shortcut-keyed). Stub regeneration after Host upgrade preserves the unsigned-shortcut + signed-target relationship.

## 10. Host Flows

### 10.1 Install / first launch (recipient has no Hub)

1. Recipient receives `coach.muragent` via any channel (email, AirDrop, USB, Slack file).
2. Double-click. OS does not recognize the extension → prompts file association.
3. (Alternative) Recipient visits `mur.run/get`, downloads `MUR Agent Host.dmg`, installs.
4. **Host installer enforces `/Applications` placement** (not `~/Applications`). On launch from `~/Applications` or `~/Downloads`, Host prompts the user: "Move to /Applications? (Required for deep-link delivery.)" Refusing leaves Host usable but URL scheme dispatch and `mur-agent-coach://...` deep links are unreliable. The installer makes the canonical choice the default.
5. On first launch (from `/Applications`), Host:
   - Registers itself as default handler for `.muragent` files via Info.plist + `lsregister -f`.
   - Registers wildcard `muragent-*` URL scheme prefix (macOS LSHandlers list; Windows registry; Linux desktop file).
   - Offers to import any `.muragent` the user drags into the dashboard.
   - Runs `mur agent doctor` to verify SMAppService availability + Gatekeeper status, surfaces results in a one-time onboarding panel.

> **Why `/Applications` is hard-required on macOS:** Tauri 2's deep-link plugin documentation and several upstream issues confirm that LaunchServices URL scheme handlers in `~/Applications` work for ad-hoc use but are unreliable for cold-start delivery, dev-mode testing, and post-update scheme re-registration. Stubs are exempt from this because they invoke Host through direct `execv` on the path recorded in `~/.mur/host_path` (§5.2), not through LaunchServices. See [Tauri deep-linking plugin docs](https://v2.tauri.app/plugin/deep-linking/) and [FabianLars/tauri-plugin-deep-link](https://github.com/FabianLars/tauri-plugin-deep-link).

### 10.2 Import (recipient already has Hub)

```
1. User double-clicks coach.muragent (or drags into Hub dashboard)
2. OS dispatches to Hub
3. Hub opens import confirmation dialog:

   ┌─────────────────────────────────────────────────────┐
   │ Install agent "Coach"?                              │
   │                                                     │
   │ ┌──── Trust ──────────────────────────────────────┐ │
   │ │ First-time author                               │ │
   │ │ Fingerprint: 6f3a...c2e1                        │ │
   │ │ (v2 will show "✓ mur verified" here if signed)  │ │
   │ └─────────────────────────────────────────────────┘ │
   │                                                     │
   │ Requires:                                           │
   │   • MUR Agent Host v2.13+                           │
   │   • npx (for context7 MCP server) — found           │
   │                                                     │
   │ ☐ Start automatically at login                      │
   │ ☐ Create desktop launcher (Dock icon)               │
   │                                                     │
   │ [ Inspect ]   [ Cancel ]   [ Install ]              │
   └─────────────────────────────────────────────────────┘

4. On Install:
   a. Extract payload to ~/.mur/agents/coach/
   b. Insert into local trust store as `pending`
   c. If "desktop launcher" checked: generate stub via §5 procedures, then run validation gate (§12.4) — fail-fast if `spctl --assess` or `codesign --verify` rejects the stub
   d. If "start at login" checked: write launchd plist with `AssociatedBundleIdentifiers = ["run.mur.host"]` so Login Items UI groups under Host (see §3.5). On macOS 13+, additionally call `SMAppService.agent(plistName:)` if the plist is reachable from inside the Host bundle.
   e. Hub focuses to Coach's window
```

### 10.3 Update (recipient receives newer `.muragent` for an already-installed agent)

```
1. Hub matches by agent.original_uuid AND signatures.author.public_key
2. If both match: offer "Update Coach: v2.13.0 → v2.14.0"
3. If uuid matches but pubkey differs: refuse to update, treat as new author
   (UI: "This agent has the same name but a different signing key — treat as new author?")
4. On update: replace profile.yaml + assets; preserve ~/.mur/agents/coach/data/
   (chat history, user-edited settings)
```

### 10.4 Uninstall

```
Right-click agent in Hub → Remove
  ↓
Confirm dialog: [ ] Delete chat history and user data
  ↓
Hub:
  1. Stops sidecar: launchctl unload ~/Library/LaunchAgents/run.mur.agent.coach.plist
  2. Removes launchd / systemd / Run registry entry
  3. Removes stub bundle / .lnk / .desktop file
     + lsregister -u <stub> on macOS to drop URL scheme registration
     + xdg-mime --remove on Linux
  4. If user opted to delete data: rm -rf ~/.mur/agents/coach/
     Else: leave ~/.mur/agents/coach/ in place for future reinstall
  5. Remove or downgrade trust store entry depending on user choice
```

### 10.5 Error paths

| Condition | UX |
|---|---|
| Tarball corrupted | "File is corrupted. Ask the author to re-share." Fatal (§7.5). |
| Schema newer than Hub | "Requires MUR Hub v2.15+. [ Check for Updates ]" |
| Schema is anything other than `mur-agent/2` (incl. legacy `mur-agent-package/1`) | **Fatal**. "Unsupported package format." No auto-upgrade path. |
| Same agent already installed (same uuid + same pubkey) | Offers Update flow |
| Same uuid, different pubkey, **with rotation manifest** (§7.1.1) | "Author rotated their signing key on `<date>`" — accepted automatically |
| Same uuid, different pubkey, **without rotation manifest** | **Hard refuse** (§7.1). User must explicitly remove old trust entry before importing. Not a click-through. |
| Missing MCP host tool (e.g., `uvx` not installed) | Imports successfully; chat window shows a one-click install hint pointing to platform-appropriate package manager |
| Signature invalid (DSSE structure broken, Ed25519 fails verify, subject hash mismatch) | **Fatal**. "This file has been modified or is not a valid mur agent." No override. (§7.5) |
| Disk space too low | Pre-flight check; abort before extraction |
| Trust store says rejected | Refuse import; user can clear rejection from preferences |
| V2: `revocations.json` lists manifest hash or author pubkey | **Fatal**. "This agent has been revoked. Reason: `<reason>`." Display revocation timestamp. No override. |
| V2: `revocations.json` expired (past `expires_at`) | Refuse all imports until refresh succeeds. "Trust list expired. Connect to refresh." |

## 11. Migration

**No migration is required or supported.** Per §1.1, neither the previous `.app` export format nor the `.murpkg` v1 package format has production users. This spec ships `.muragent` v2 as a clean break — Hub and Commander accept only the v2 schema; any earlier artifact is rejected with the §10.5 fatal "Unsupported package format" error.

The existing `mur agent migrate-to-hub` subcommand (per `2026-05-11-mur-hub-companion-design.md` M-h7) is unaffected — it handles **in-place** migration of a single user's local `~/.mur/agents/` layout when they install Hub on top of their existing CLI install. It does not interact with `.muragent` files at all.

## 12. Testing Strategy

### 12.1 Tests preserved from existing pipeline

These tests stay in their current location, gated behind a CI flag that activates only when `MUR_APPLE_DEVELOPER_ID` and notarize credentials are present (so the `--standalone` path is exercised on the release builder but skipped on local devs without signing creds):

- `mur-core/tests/agent_export_macos.rs`
- `mur-core/tests/agent_export_gui_url_scheme.rs`
- `mur-core/tests/agent_export_gui_nsservices.rs`
- `mur-agent-gui/src-tauri/tests/send_url_scheme.rs`

The `parse_share_url` test in particular **does not change** — the URL format `muragent-<slug>://share?...` is preserved end-to-end on the Hub stub path. There is no compat test for `.murpkg` v1; that format is dead per §1.1.

### 12.2 New test suites

| Suite | Coverage |
|---|---|
| `mur-common/tests/muragent_format.rs` | Manifest schema v2, `manifest.signed.json` derivation (JCS canonicalization), reject YAML anchors / aliases / merge-keys / non-string keys / native timestamps |
| `mur-common/tests/muragent_dsse.rs` | DSSE PAE byte-exact construction, multi-signature envelope round-trip (author + simulated mur sig), `verify_strict` on small-order points |
| `mur-common/tests/muragent_statement.rs` | In-toto v1 Statement shape, subject list completeness vs tarball contents, NFC path normalization, reject `\x00` / control chars / `..` in paths |
| `mur-common/tests/muragent_surface_blocks.rs` | `hub:` / `commander:` block parsing; unknown blocks ignored not rejected; `required_surfaces` validation; `optional_capabilities` feature-flag semantics |
| `mur-common/tests/muragent_executable_ban.rs` | Each forbidden case in §6.4 step 2 rejected with specific error code: case-insensitive extension block (`.DLL`, `.So`, `.EXE` all caught), versioned `.so.1`/`.so.0.1.0` caught, `.elf`/`.wasm` caught, interpreter `-e`/`-c`/`--eval` flags caught (`ruby -e`, `perl -e`, `php -r`, `node -e`, `lua -e`, `awk ... system()`), shell metacharacter chains rejected, `pip install &&` / `npm install -g &&` chains rejected, execute bits on regular files rejected (directories with execute bit OK) |
| `mur-common/src/muragent/executable_ban.rs` (unit tests) | MCP permit-list: known-safe commands (`uvx`, `npx`, `docker`, `gh`) pass; unknown commands (`my-custom-tool`) warn but don't reject; permit-list gating exercises all v1 safe-list entries |
| `mur-common/tests/muragent_legacy_reject.rs` | Any non-`mur-agent/2` schema (incl. `mur-agent-package/1`) is rejected with the §10.5 fatal "Unsupported package format" error code |
| `mur-common/tests/muragent_key_rotation.rs` | Rotation manifest dual-signature verify; trust store entry transitions `known → superseded`; importing artifact signed by old key after rotation still verifies but flagged as superseded source |
| `mur-common/tests/muragent_trust_hard_refuse.rs` | Known author, key changed, NO rotation manifest → hard refuse with no override path |
| `mur-common/tests/muragent_fatal_not_advisory.rs` | Every §6.4 failure path returns error, never falls through to import. Property test: any byte-flip in `signatures.json` causes refuse; any byte-flip in `manifest.signed.json` causes refuse; any tarball file content tamper causes refuse. |
| `mur-common/tests/trust_store_concurrent.rs` | Hub and Commander both write `~/.mur/trust/trust.yaml` under file lock; concurrent reads non-blocking; lock timeout retries cleanly; legacy `~/.mur/trust.json` → `~/.mur/trust/trust.yaml` migration runs exactly once |
| `mur-hub-gui/src-tauri/tests/stub_generation.rs` | macOS stub `.app` created with correct Info.plist (bundle id, URL scheme, NSServices), ad-hoc resigned, `host_version.txt` written, `lsregister -f` + `LSSetDefaultHandlerForURLScheme` + `pbs -update` called in order |
| `mur-hub-gui/src-tauri/tests/stub_generation_win.rs` | Windows `.lnk` shape (absolute Target path, no relative components), per-agent registry tree under `HKCU\Software\Classes\muragent-<slug>`, `IApplicationAssociationRegistration` invoked |
| `mur-hub-gui/src-tauri/tests/stub_generation_linux.rs` | `.desktop` reverse-DNS file ID, `StartupWMClass`, `StartupNotify=true`, `update-desktop-database` + `xdg-mime default` invocation, `mimeapps.list` NOT touched directly |
| `mur-hub-gui/src-tauri/tests/per_agent_ipc.rs` | Bind / connect protocol on `~/.mur/agents/<slug>/ipc.sock`; URL forwarding to existing instance; mode-0600 enforcement; mismatched-URL-scheme payload rejected |
| `mur-hub-gui/src-tauri/tests/stub_self_update.rs` | Stale-stub detection on Host startup (version-skew triggers regenerate); idempotent regeneration; sign-validation gate runs on regenerated stub |
| `mur-hub-gui/src-tauri/tests/trust_store.rs` | First-time → pending; second import same key → silent; rotation manifest path; word-list fingerprint deterministic |
| `mur-hub-gui/src-tauri/tests/first_time_prompt_ui.rs` | Snapshot test: prompt body ≤ 4 lines, no "always trust" checkbox, declared permissions surfaced, primary button label is "Import" not "Run anyway" |
| `mur-hub-gui/src-tauri/tests/single_instance_dispatch.rs` | Second `mur-agent-host --agent X` invocation forwards to running instance via per-agent IPC channel, not Tauri's global single-instance plugin |

### 12.3 Integration test

`mur-core/tests/integration/export_install_roundtrip.rs`:

1. Export agent `coach` via `mur agent export coach --format=muragent`.
2. Verify resulting `.muragent` passes all §6.4 validation.
3. Spawn Hub in test mode, invoke `mur agent install coach.muragent`.
4. Verify `~/.mur/agents/coach/` populated.
5. Verify stub exists in platform-appropriate location.
6. Verify sidecar launchable.
7. Tear down: `mur agent uninstall coach`.
8. Verify all artifacts removed.

### 12.4 Gatekeeper validation gate (macOS)

After stub generation (§5) ad-hoc resigns the launcher, the install code path MUST validate both signature and Gatekeeper acceptance before returning success:

```
codesign --verify --deep --strict --verbose=4 <stub>.app
spctl --assess --type execute --verbose=4 <stub>.app
```

Either failure aborts install with a specific error code distinguishable from "signing failed entirely". This catches the 2022 Chromium PWA breakage (designated-requirement mismatch after re-sign — Chromium issues 1281111 / 1297588) before it reaches users.

Add a CI job that runs on a clean Sonoma + Sequoia VM to catch macOS version regressions:

```
test/macos_stub_gatekeeper.sh:
  generate stub
  assert codesign --verify exits 0
  assert spctl --assess exits 0
  assert execv against ~/.mur/host_path (smoke test) succeeds
```

The launcher binary itself is fixed across stubs (same bytes ad-hoc signed in place), so once a Sonoma/Sequoia CI pass is green, per-agent generation is not at risk of per-instance failures. Re-run the CI job whenever the launcher binary, its signing entitlements, or the stub-generation code changes.

## 13. Performance Targets

| Operation | Target | Surface |
|---|---|---|
| `mur agent export coach --format=muragent` (cold) | < 5 s (no `cargo build`) | Hub-side CLI |
| `murc agent export coach` (Commander-side export, no UI assets) | < 3 s | Commander-side CLI |
| `mur agent install coach.muragent` (data extraction + stub generation, no Hub UI) | < 2 s | Hub |
| `murc agent install coach.muragent` (data extraction + workflow loading, no chat platform reconnect) | < 1 s | Commander |
| Hub cold launch on import double-click | < 3 s on M-series Mac, < 5 s on Intel | Hub |
| Stub URL scheme dispatch (`open muragent-coach://...`) | < 500 ms end-to-end | Hub |
| Sidecar memory per agent (idle) | < 50 MB RSS | Both |
| DSSE signature verification (single `.muragent`, ~5 MB) | < 100 ms on M-series | Shared library |
| Trust store read (steady state) | < 5 ms | Both |

Status quo `mur agent export coach --gui` measures 90–180 s on M-series with all signing creds set, primarily `cargo build` and notarytool.

## 14. Open Questions

All four originally-flagged decision questions (B1–B4) have been resolved and incorporated above. Remaining for follow-up:

1. **Stub regeneration on Hub upgrade.** If Hub v2.14 changes the stub launcher binary, do we regenerate every existing stub on first launch? Likely yes, with a manifest version field in each stub's `Resources/agent.txt`.
2. **macOS LaunchAgent label collisions** with users who installed an agent under both the old per-agent `.app` and the new Hub model. Detection logic in §11.2 should disambiguate, but the exact dedup heuristic needs an integration test.
3. **Windows EV cert decision — resolved.** Switch to Microsoft Trusted Signing (§9.4); EV certs no longer bypass SmartScreen as of March 2024. Trusted Signing is cheaper, cloud-managed, and accumulates reputation faster at our scale.
4. **Linux per-distro autostart** — `.config/autostart` works on GNOME / KDE / XFCE; tiling WMs handle it but render no Dock surface. Acceptable v1 limitation, document in release notes.
5. **Future SMAppService-native path on macOS.** Moving `mur-agent-runtime` from `~/.mur/bin/` into `MUR Agent Host.app/Contents/Helpers/` would enable full SMAppService integration (cleaner Login Items UX, future-proof against macOS deprecating user-LaunchAgent plists). Conflicts with the current BusyBox-style symlink architecture. Reserved for v2 / a dedicated platform-restructure spec; v1 ships with the `AssociatedBundleIdentifiers` compromise (§3.5).
6. **Wayland focus semantics on Linux.** Single-instance activation on Wayland depends on the per-agent IPC (§5.3) — there is no external `wmctrl`-equivalent. KDE Plasma 6.8 is removing X11; testing matrix for Sequoia-era Linux desktops needs to cover GNOME 45+, KDE Plasma 6+ Wayland. Not blocking v1 but a known constraint.
7. **`revocations.json` distribution channel** — V2 will fetch from the Hub update channel (§7.4.1). Exact endpoint, signing key custody, and refresh cadence to be specified in the V2 trust-badge spec when written. v1 leaves the embedded root pubkey set (§7.3) populated but unused, ensuring forward compatibility.
8. **Sigstore migration path.** The DSSE envelope (§6.3) is byte-compatible with Rekor entries. If we outgrow `revocations.json` at scale, migrating to Sigstore transparency-log monitoring requires only the verifier change (recipients query Rekor); the on-disk format does not change. No action in v1.
9. **`mur-common::canonical` vs `mur-common::jcs` split.** The existing `canonical.rs` explicitly does NOT implement RFC 8785 JCS. `.muragent` requires full JCS compliance (number serialization rules, no scientific notation). Decision: create a separate `mur-common::jcs` module for `.muragent` rather than upgrading `canonical.rs` (which would break existing identity rotation signatures). The two canonical forms serve different contracts — document this explicitly so future contributors don't try to "unify" them.
10. **`mur-common::muragent` as shared library across repos.** The package reader/writer/validator lives in the mur repo's `mur-common` and is consumed by Commander via Git dependency. This means Commander's `murc agent export` depends on a pinned mur repo commit. The release coordination overhead (update Commander's Cargo.toml rev when muragent format changes) is acceptable for v1. If it becomes a friction point, extract `mur-common::muragent` into a standalone crate published to crates.io.
11. **macOS quarantine xattr behavior on future macOS versions.** The spec relies on "stubs created by locally-signed code don't get quarantine xattr." Apple has progressively tightened bundle creation policies (Ventura → Sonoma → Sequoia). The §12.4 Gatekeeper validation gate catches regressions, but if a future macOS version starts quarantining locally-created bundles, all existing stubs fail at once and the self-update mechanism (§5.4) must bootstrap from a broken state. Mitigation: Host should run `spctl --assess` on each stub during startup health check (not just version comparison) and regenerate proactively on failure.

## 15. Implementation Order (preview, full plan in writing-plans skill)

Phased rollout to keep CI green throughout:

1. **M-export-1** — `mur-common::muragent` shared library (writer + reader + validator). `mur-common::jcs` RFC 8785 canonical JSON module (separate from existing `canonical.rs`). DSSE envelope, in-toto Statement with `manifest_sha256` predicate, manifest schema with `hub:` / `commander:` / `required_surfaces:` / `optional_capabilities:` and reserved `deployment:` / `assignment:` blocks. Property-test suite for fatal-not-advisory contract. MCP command permit-list and validation rules (§6.4 step 2).
2. **M-export-2** — `mur agent export <name>` switches default to `.muragent` (no deprecation phase per §8.2). `mur agent install / uninstall / inspect` CLI surface. Trust store data layer at `~/.mur/trust/`.
3. **M-export-3** — Hub-side import dialog (§7.2 design), shared trust store integration, first-time-author prompt UI snapshot tests, surface-block reading (Hub picks `hub:` block).
4. **M-export-4** — Per-platform stub generation (macOS first with Gatekeeper validation gate §12.4, then Windows with `IApplicationAssociationRegistration`, then Linux with `xdg-mime`/`update-desktop-database`). Per-agent IPC layer (§5.3). Microsoft Trusted Signing for the Windows Host installer.
5. **M-export-5** — Per-platform autostart wiring: launchd plist with `AssociatedBundleIdentifiers`, Windows Run registry, systemd `--user` unit. Stub self-update flow (§5.4). Key rotation manifest support (§7.1.1). **Sidecar lifecycle migration:** swap Hub's child-process supervisor (`mur-gui-core/src/sidecar.rs`) for OS init system ownership (§3.1). **Expression engine relocation:** move expression evaluation from Hub into `mur-agent-runtime` sidecar so C6/C7/D1 features function without Hub open (§3.1 migration note).
6. **M-export-6 (cross-repo)** — `mur-commander` consumes `mur-common::muragent` at the pinned commit; adds `murc agent install / export` that respects `commander:` blocks; both surfaces interoperate on `~/.mur/trust/` and `~/.mur/agents/`. Coordinated release in the Commander repo.
7. **M-export-7** — `revocations.json` consumer scaffolding shipped in both surfaces (issuer infrastructure remains V2 work; v1 only ships the consumer so the format is forward-compatible).

Standalone path (`--standalone`) is touched only by M-export-2 (flag wiring + hard-error for the removed `--gui`); the 13-phase pipeline itself is unmodified.

## 16. Multi-Surface Architecture

This section consolidates how `.muragent` v2 fits between the two surfaces in the MUR ecosystem. It is normative for the v1 implementation and is the long-form companion to §3.8.

### 16.1 The two surfaces

| | **MUR Hub** | **MUR Commander** |
|---|---|---|
| Repo | `~/Projects/mur` (this repo) | `~/Projects/mur-commander` (separate repo) |
| Version line | v2.13.x | v0.10.x |
| Binary set | `mur` (CLI), `mur-agent-runtime` (sidecar), `mur-hub-gui` (Tauri 2 desktop app), `mur-daemon` | `murc` (CLI), `mur-daemon` (Commander's own daemon), `mur-gateway` (Slack/TG/DC handler), `mur-web` (dashboard on :3939), `mur-supervisor` |
| User-visible surface | Per-agent windows, Dock icons, companion pet, voice | Slack / Telegram / Discord messages, web dashboard, workflows, programs, Jira |
| Lifecycle owner | macOS launchd / Linux systemd-user / Windows Run (per-agent sidecars); Hub UI on demand | Single `mur-daemon` process (Docker or native), gateway maintains chat platform connections |
| Distribution | `brew install --cask mur-hub`; `MUR Agent Host.dmg` from mur.run/get | `brew install mur-run/tap/mur-commander`; `docker pull murrun/mur-commander`; `curl install.mur.run \| sh` |
| Reads `.muragent` | YES (this spec) | YES (this spec) |
| Writes `.muragent` | YES via `mur agent export` | YES via `murc agent export` (mirror of mur subcommand; same crate as `mur-common`-level package writer) |
| Reads / writes shared trust store at `~/.mur/trust/` | YES | YES |
| Reads / writes shared agent dir at `~/.mur/agents/<slug>/` | YES | YES |
| Signal protocol (`mur-common::Signal`) | Receives signals from Commander via `~/.mur/inbox/` or HTTP `POST /v1/signals/batch` on `mur-daemon` | Emits signals (C1 evidence, C2 chat-extraction, C3 procedural) via outbox |

Both products consume the **same** `mur-common` crate (Signal envelope, AgentProfile types, `.muragent` reader/writer). Splitting the package reader into `mur-common::muragent` makes that shared library the single source of truth for the format.

### 16.2 Surface-specific blocks: who reads what

```
.muragent
├── manifest.yaml
│   ├── agent: <slug, display_name, bundle_id, url_scheme, original_uuid>  ← BOTH
│   ├── required_surfaces: [hub | commander | both]                        ← BOTH (validation)
│   ├── optional_capabilities: [voice, workflow_engine, ...]               ← BOTH (feature-flag)
│   ├── profile: <ref to profile.yaml>                                     ← BOTH
│   ├── mcp_servers: [...]                                                 ← BOTH
│   ├── icon: <hashes>                                                     ← BOTH (Hub renders Dock; Commander uses as chat avatar)
│   ├── hub:                                                               ← Hub only
│   │   └── appearance / voice / pet / url_scheme_overrides
│   └── commander:                                                         ← Commander only
│       └── chat_platforms / workflows / programs / jira / sub_agents
├── profile.yaml                                                           ← BOTH
├── voice/voice.yaml                                                       ← Hub only (if hub.voice.enabled)
└── assets/commander/                                                      ← Commander only
    ├── workflows/*.yaml
    └── programs/*.md
```

**Validation rules in `mur-common::muragent::Validator`:**

1. If `required_surfaces` contains `hub`, validator requires `hub:` block present.
2. If `required_surfaces` contains `commander`, validator requires `commander:` block present.
3. If `required_surfaces` is `[hub, commander]`, both blocks required.
4. If `required_surfaces` is empty or omitted, defaults to `[hub]` (the historically dominant surface).
5. Unknown blocks under `manifest.yaml` (e.g., a future `mobile:`) → warn, do not reject. This is the forward-compat rule.
6. Surface-specific block referencing a file in `assets/<surface>/` that does not exist in the tarball → reject (broken manifest).

**Reader posture:** each surface reads its own block plus shared fields. Unknown blocks are silently skipped. Capability gating uses `optional_capabilities`: a surface that doesn't implement a listed capability shows a one-time "Feature `voice` requires Hub v2.14+" hint but does not refuse the import.

### 16.3 Same identity across surfaces

An agent's identity is the Ed25519 keypair at `~/.mur/agents/<slug>/identity.key` (existing — unchanged from `AgentProfile`). When a recipient imports `coach.muragent`:

```
~/.mur/agents/coach/
├── identity.key                ← restored from author's identity (private key NEVER in .muragent)
├── identity.pub                ← restored from manifest.agent for verification
├── profile.yaml                ← from profile.yaml in tarball
├── hub.yaml                    ← extracted from manifest hub: block (Hub reads)
├── commander.yaml              ← extracted from manifest commander: block (Commander reads)
├── workflows/                  ← extracted from assets/commander/workflows/
└── programs/                   ← extracted from assets/commander/programs/
```

A recipient who imports through Hub: Hub writes `hub.yaml` and starts the runtime sidecar. If recipient later installs Commander on the same machine: Commander finds `commander.yaml` already in place and starts using it without re-import. The reverse (Commander imports first, Hub installed later) works symmetrically.

`identity.key` is generated **fresh** on first import unless the manifest is in `clone` mode (see existing `BundleMode` in `mur-common::bundle`). The published `.muragent` carries only `identity.pub` for verification continuity; the recipient mints a new private key that re-signs all outgoing artifacts.

### 16.4 Signal protocol (runtime channel) is orthogonal

`mur-common::Signal` (frozen 2026-05-18) carries runtime feedback **between** surfaces. `.muragent` carries install-time configuration. They never conflict:

```
Author exports .muragent  ─→  Recipient's Hub imports                   ─┐
                                                                          │
                          ─→  Recipient's Commander imports              ─┤
                                                                          │
Runtime:                                                                  │
  Commander runs workflow → Signal {kind: C1Evidence, …}        ────────→ ├─→ mur-daemon inbox → pattern Evidence updated
  Commander extracts pattern from Slack → Signal {kind: C2Draft, …} ────→ │
  Commander analyzes audit log → Signal {kind: C3Procedural, …}    ────→ │
                                                                          │
  Hub user dismisses suggestion → Signal {kind: Override, …}    ────────→ ┘
```

Surfaces do not need to be running simultaneously: Commander writes to `~/.mur/commander/outbox/` (atomic file drops, per the 2026-05-18 wire-protocol spec §5.3); `mur-daemon` polls it. If `mur-daemon` is not running, the outbox accumulates and is processed on next start. This is delay-tolerant by design (per the 2026-04-18 commander memory-sync spec and 2026-05-18 wire-protocol freeze).

### 16.5 Distribution stays separate

This spec does NOT propose unifying the Hub and Commander installers. Reasons:

1. Commander has real users on Slack/TG/DC; changing their install flow is gratuitous risk.
2. Tauri 2 (Hub) and headless daemon (Commander) have different runtime profiles, build matrices, and OS integration surfaces. Forcing one installer to ship both inflates download size and complicates each release.
3. Industry pattern (Linear, Slack, Discord, GitHub Desktop + gh + Actions): separate clients under one brand, distinct distribution.

Both projects MAY publish to the same Homebrew tap (`mur-run/tap`) as separate formulae. A meta-formula `mur-run/tap/mur-suite` could install both for users who want the full experience — documented as convenience, not as the default.

### 16.6 `mur-common::muragent` shared library

To prevent format drift between the two repos, the package reader/writer lives in `mur-common::muragent` (this repo). The `mur-commander` repo consumes `mur-common` as a Git dependency pinned to a specific commit:

```toml
# mur-commander/crates/engine/Cargo.toml
[dependencies]
mur-common = { git = "https://github.com/mur-run/mur", rev = "<sha>" }
```

Format changes (new optional fields, new validation rules, future schema bumps) happen in this repo; `mur-commander` pulls the new commit when it wants to support them. This is the existing pattern for `Signal` and `SignedEnvelope` — extend it to `.muragent`.

Breaking schema changes (`mur-agent/2` → `mur-agent/3`) require coordinated releases of both repos, gated by the version negotiation in `manifest.exporter.min_*_version`.

### 16.7 Commander agent discovery (cross-Commander sharing)

> **Status: future — not implemented.** Nothing in this section exists in code yet; it records the reserved design only.

The design above covers Hub↔Hub and Hub↔Commander sharing through the `.muragent` file. For Commander↔Commander sharing — where a Commander user wants to share an agent with another Commander user without involving Hub — three additional mechanisms are defined.

#### 16.7.1 URL-based install

Commander accepts `.muragent` files from URLs, enabling serverless sharing:

```
murc agent install https://example.com/coach.muragent
murc agent install https://agent.mur.run/david/coach/muragent   # future Agent Directory
```

Commander downloads the URL, verifies the DSSE signature, and imports exactly as if the file were local. URL install supports `--auto-start` and all other recipient flags from §8.3. The download is streamed to a temp file (no in-memory buffering of large tarballs); verification runs on the temp file before extraction.

#### 16.7.2 Agent Directory (V2, URL format reserved now)

A public Agent Directory at `https://agent.mur.run/` serves as an optional discovery surface. The URL scheme is reserved in v1 so that Commander and Hub can link to it immediately:

```
https://agent.mur.run/<author>/<slug>/muragent    # latest version
https://agent.mur.run/<author>/<slug>/v2.13.0     # version-pinned
```

The directory is indexed by author pubkey and agent slug. Listing/search endpoints are defined in a separate Agent Directory spec. v1 Commander ships with a placeholder `/agent directory` command that prints the URL scheme and notes "coming in v2."

#### 16.7.3 A2A AgentCard `package_url` extension

Commander's existing A2A agent discovery (`crates/engine/src/a2a/discovery.rs` in the `mur-commander` repo) discovers *running* agent services from well-known URLs. To bridge discovery to installation, add an optional `package_url` field to the A2A AgentCard:

```json
{
  "agent_card": {
    "name": "Coach",
    "description": "Daily standup coach",
    "package_url": "https://agent.mur.run/david/coach/muragent",
    "package_sha256": "<hex>",
    "capabilities": { ... }
  }
}
```

When Commander discovers an agent via A2A, it can offer one-click install if `package_url` is present. The `package_sha256` provides integrity verification independent of the DSSE envelope (defense-in-depth: even if the URL host is compromised, the manifest signature in the downloaded `.muragent` must still verify against a known author key).

#### 16.7.4 Commander peer share command

On the Commander side, sharing an installed agent with another Commander user on the same chat platform:

```
/agent share coach with @bob
```

Commander sends a chat message to @bob containing the `.muragent` as a file attachment (Slack: `files.upload`; Telegram: `sendDocument`; Discord: attachment). If the file is too large for the platform's limit, Commander uploads to a user-configured object store (S3 bucket, R2, or the future Agent Directory) and sends the URL instead. The recipient's Commander sees the file/URL and offers to install.

---

## 17. Deployment and Assignment Extension Points

This section defines reserved manifest blocks and CLI surfaces for **agent deployment** (push to remote machine) and **agent assignment** (assign to a specific machine or user). These are NOT implemented in v1 — they are extension points reserved in the `.muragent` format so that future specs can add these capabilities without a schema-breaking change.

### 17.1 Why deployment and assignment are separate concerns

The v1 `.muragent` solves **export and packaging**: producing a portable, signed agent artifact that can be shared and installed. Two closely-related capabilities are explicitly deferred:

| Capability | v1 Status | What's missing |
|---|---|---|
| **Export/publish** | ✅ Covered | `mur agent export --format=muragent` |
| **Package as portable unit** | ✅ Covered | `.muragent` tar.gz + DSSE signature |
| **Deploy to remote** | ❌ Deferred | Remote machine identity, transport layer, remote launch, health reporting, rollback |
| **Assign to machine/user** | ❌ Deferred | Machine/user registry, assignment semantics, lifecycle management, ACL integration |

Deployment and assignment require infrastructure that does not exist in v1: device identity (per-machine keypair), a secure transport channel (mTLS, SSH, or WireGuard), a device registry (which machines does the user control?), and integration with Commander's existing seats/roles ACL. These are product-level decisions that span both surfaces and the cloud infrastructure.

### 17.2 Reserved manifest blocks

The following blocks are reserved in `manifest.yaml` for future use. v1 validators MUST ignore them (forward-compat rule per §16.2). Future specs define their schemas:

```yaml
# ─── Deployment (future spec) ─────────────────────────────────────────────
deployment:                          # IGNORED in v1; reserved
  target_os: [macos, linux, windows] # which OS the agent supports when deployed
  min_ram_mb: 512
  min_disk_mb: 200
  required_binaries:                 # binaries that must exist on target
    - npx
    - uvx
  post_install:                      # post-install script (constrained: must use
    command: systemctl               #   permit-listed commands only)
    args: ["--user", "restart", "mur-agent-coach"]

# ─── Assignment (future spec) ─────────────────────────────────────────────
assignment:                          # IGNORED in v1; reserved
  mode: claim                        # claim | invite | push
  target_label: staging              # label selector for target machines
  auto_start: true
  revoke_on: 2026-12-31T23:59:59Z    # optional expiry
```

### 17.3 Device identity (prerequisite for deployment)

Deploying an agent to a remote machine requires the machine to have an identity that the sender can authenticate. The prerequisite infrastructure (not in this spec):

```
~/.mur/device/
├── device.key              # Ed25519 device keypair (generated on first run)
├── device.pub              # public key
└── device.yaml             # device metadata (hostname, OS, capabilities)
```

The device keypair is generated once per machine (by Hub or Commander, whichever installs first) and registered with the user's mur account (V2 cloud component). Remote deployment targets this device identity. The `.muragent` itself does not carry deployment target information — the deployment command references device identities from the registry.

### 17.4 CLI surface (future)

```
# Deployment (future spec)
mur agent deploy <name> --to <device-id>            # push to a registered device
mur agent deploy <name> --to-label <label>           # push to all devices matching label
mur agent deploy <name> --to-user <commander-user>   # push to all devices owned by a Commander user
mur agent deploy status <deployment-id>               # check deployment health
mur agent deploy rollback <name> --to <device-id>     # rollback to previous version

# Assignment (future spec)
mur agent assign <name> --to machine:<device-id>      # assign to specific machine
mur agent assign <name> --to user:<commander-user-id> # assign to Commander user
mur agent assign revoke <name> --from <target>        # revoke assignment
mur agent assign list <name>                          # list current assignments
```

### 17.5 Implementation sequencing

1. **This spec (v1):** `.muragent` format, export/import, trust model, Hub stubs, Commander `commander:` block consumption.
2. **Agent Directory spec (v1.5):** public directory at `agent.mur.run`, search, version listing.
3. **Device Identity spec (v2 prerequisite):** `~/.mur/device/`, device registration, cloud account binding.
4. **Deployment spec (v2):** remote push, health reporting, rollback, deployment ACL.
5. **Assignment spec (v2):** machine/user assignment, lifecycle, Commander seats/ACL integration.

The `deployment:` and `assignment:` manifest blocks reserved above ensure that when these specs are written, the `.muragent` format requires no breaking change.

---

**End of design.**
