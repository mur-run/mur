# MuR Agent Export UX — "Give an Agent to a Friend" (v1)

**Date:** 2026-05-29
**Status:** Design (ready for implementation plan)
**Builds on:** `2026-05-20-agent-export-host-data-model-design.md` (Two-Surface architecture, `.muragent` v2, trust model), `2026-05-11-mur-hub-companion-design.md` (Hub), `2026-04-29-model-registry-and-secret-refs-design.md` (`~/.mur/models.yaml`).
**Pillar of:** `2026-05-29-mur-strategy-positioning-vs-archon.md` §4.1 (consumer companion-app wedge) — this is the first pillar brainstormed out of that umbrella strategy.

---

## 1. Problem & reframe

The strategy doc names "export your agent as a self-contained app you can give to a friend" as the consumer wedge, and points at the **embedded binary** mechanism (`mur-agent-runtime/export/bin_embed.rs`, `include_bytes!`). A code census on 2026-05-29 shows that framing is the *inferior* path, and that a better architecture was already designed and largely built:

- The **`2026-05-20` Two-Surface design** already decided the right model: the **Host (Hub) app is signed once** with mur's own Developer ID; **`.muragent` is pure data (no executable code)**; per-agent OS identity (file association, Dock/taskbar) is expressed through **locally-generated stubs** created by the already-signed Host, so they never receive a quarantine flag and Gatekeeper/SmartScreen never gates them (the "Chrome PWA trick", §3.2–3.4 of that spec). This dodges **both** the build-toolchain problem and the code-signing problem that the embedded-binary path runs into.
- Much of that substrate is **already implemented** (see §3). The remaining work is not new architecture — it is **completing the end-to-end give-to-a-friend UX** on top of the existing substrate.

**This spec scopes that completion.** It does not redesign the package format, trust model, or runtime topology — those are inherited from `2026-05-20`.

### Why not the embedded single-file binary?

A single file that runs with nothing installed must embed the runtime. Appending a payload to a signed Mach-O (macOS) or PE (Windows) binary **breaks its code signature**, so the recipient hits "unidentified developer / damaged file" and must perform manual override steps — the exact friction this product is trying to remove. A truly smooth zero-install single file would require **per-export notarization** (a cloud signing service: cost, latency, network — contrary to the local-first/free ethos). The smooth single-file giveaway is therefore the **`.muragent` file + a one-time Host install** (like a document + its app), not an embedded executable. The embedded single-file binary is explicitly **deferred** (§12).

---

## 2. Goals & Non-Goals (v1)

### Goals

1. A producer can turn one of their agents into a single shareable file (`coach.muragent`) **from the Hub GUI** (and CLI) with **no build toolchain** and **no per-export signing**.
2. A recipient who has never used MuR can install the Host once, **double-click the `.muragent`**, and reach a working conversation in **under 5 minutes**.
3. On first run the recipient is guided through **model resolution** that is **local-first but cloud-honest** (§7), because model weights never travel in the package.
4. The package never leaks the producer's private key, API keys, or other secrets.
5. Same-platform giveaway works end-to-end (producer and recipient on the same OS/arch). Cross-platform is an additive future extension, not a v1 requirement.

### Non-Goals (v1, explicitly deferred)

- Embedded single-file executable (`--format=bin` append / `include_bytes!`). See §12.
- Cross-platform export (produce a Windows package from a Mac). Architecture must not preclude it; v1 does not ship it.
- Share-by-URL (`muragent-<slug>://share?...`) as a giveaway channel — v1 ships file-based giveaway only.
- Commander cross-repo consumption (`2026-05-20` M-export-6).
- V2 trust badge, `revocations.json` issuer infrastructure.
- Bundling model weights of any size.

---

## 3. Current state (census, 2026-05-29)

| Capability | State | Evidence |
|---|---|---|
| `.muragent` v2 format + DSSE signing + reader/writer/validator/installer | ✅ built | `mur-common/src/muragent/{writer,reader,validator,installer,dsse,statement,jcs_canonical}.rs` |
| Trust store `~/.mur/trust/` + key-rotation manifests | ✅ built | `mur-common/src/muragent/installer.rs` (uses `crate::trust::{TrustStore,…}`, `~/.mur/trust/rotations/`) |
| CLI `mur agent export / install / inspect / uninstall` | ✅ built | `mur-core/src/cmd/agent/{export,install}.rs`; default format `muragent` |
| Export profile sanitization (strips private key, secretful notification targets, socket auth) | ✅ built | `sanitize_profile_for_export()`, `export.rs:89` |
| Hub recipient import (inspect + install backend for §7.2 dialog) | ✅ built | `mur-hub-gui/src-tauri/src/import_muragent.rs` |
| Per-platform stub generation | 🟡 partial | `mur-gui-core/src/stub/{macos,linux,windows}.rs` |
| First-run extraction (content-addressed cache) + MCP prereq check | ✅ built | `mur-agent-runtime/src/export/{extract,prereq_check}.rs` |
| Runtime model resolution from registry | ✅ built (partial) | `resolve_model_entry()`, `supervisor.rs:773` |
| **Host distribution + `.muragent` OS file association** | ❌ gap | `tauri.conf.json`: `productName "MuR Hub"`, `targets ["app"]`, no `fileAssociations`, no deep-link plugin, no `dmg` |
| **First-run model resolution wizard (no-backend case)** | ❌ gap | import dialog surfaces MCP/voice/pet/trust only; no model-backend resolution; `model_hint` absent from manifest |
| **Hub "Share this agent" (producer side)** | ❌ gap | Hub has import only; no export command |
| Legacy `--format=gui` 13-phase pipeline (cargo+npm+tauri) / `--format=bin` (cargo + raw-home tar → secret leak) | ❌ to retire | `agent_export_gui.rs` (13 phases), `export.rs:export_bin`, `build.rs` `append_dir_all(".", src)`, `doctor.rs::checks_for` |

The four gaps map to Workstreams **A, B, C, D** below.

---

## 4. The give-to-a-friend loop (acceptance narratives)

**Producer (Hub):** open Hub → pick agent "Coach" → **Share** → save `coach.muragent`. Instant; no toolchain; profile sanitized; signed with the producer's own author key. Send the one file by any channel.

**Recipient (no prior MuR):**
1. Receives `coach.muragent`.
2. (First time only) downloads the free **MuR Host** from `mur.run/get`, drags to `/Applications`.
3. Double-clicks `coach.muragent` → OS routes it to Host → §7.2 import dialog (trust + declared permissions).
4. On install, first run shows the **model resolution wizard** (§7): local-first recommendation, cloud when the agent needs it, manual override always available.
5. Conversation works. Total time < 5 min; no security warnings (Host is properly notarized; the package is data).

---

## 5. Workstream A — Host distribution & OS file association

Goal: a double-clicked `.muragent` reaches the already-built import backend, and the Host is distributable.

- **`mur-hub-gui/src-tauri/tauri.conf.json`:**
  - Add `bundle.fileAssociations`: extension `muragent`, mimeType `application/vnd.mur.agent`, role Viewer, icon.
  - Add the Tauri **deep-link plugin** for the `muragent-*://` scheme family (preserves the existing `parse_share_url` contract).
  - Add `dmg` to `bundle.targets` (keep `app`).
  - Configure macOS signing identity + notarization via **mur's own Developer ID**, read from CI environment (not a user step). Windows: Microsoft Trusted Signing (per `2026-05-20` §9.4).
  - Keep identifier **`run.mur.hub`** (do not churn the shipping identity; the `2026-05-20` spec's `run.mur.host` is treated as a draft name). Login-Items grouping uses `AssociatedBundleIdentifiers = ["run.mur.hub"]`.
- **Open routing** (`mur-hub-gui/src-tauri/src/lib.rs`): handle OS "open file" (macOS `RunEvent::Opened`, Windows argv, Linux `%f`) and "open URL" events → call existing `inspect_muragent_file` → render the §7.2 dialog. Single-instance so a second double-click focuses the running Host.
- **First-launch onboarding** (extend `mur-hub-gui/src-tauri/src/onboarding/`): enforce `/Applications` placement (prompt to move if launched elsewhere); register as default `.muragent` handler (`lsregister -f` on macOS, registry on Windows, `xdg-mime` + `update-desktop-database` on Linux); run `mur agent doctor`; offer drag-to-import.
- **`mur.run/get` DMG** is produced by mur's release pipeline (`build.sh` / CI), not a user command.

---

## 6. Workstream C — Hub "Share this agent" (producer side)

Goal: produce a `.muragent` from the GUI, reusing the CLI path.

- New Tauri command `export_muragent_file(name, out_path, mode)` in a new `mur-hub-gui/src-tauri/src/export_muragent.rs`, wrapping the same `MuragentWriter` + `build_manifest_from_profile` + `sanitize_profile_for_export` the CLI uses.
- Default **template mode** (mint fresh keys on the recipient, sanitized). Clone mode stays gated exactly as today (`MUR_ALLOW_UNSAFE_CLONE`, until the rekey ceremony lands).
- UI: per-agent "Share" action → native save dialog → `<slug>.muragent` → success toast ("Send this file to anyone with MuR Host").
- File-based only in v1 (share-URL deferred, §2).

---

## 7. Workstream B — First-run model resolution wizard (the new UX)

Model weights never travel; the agent's binding (`AgentProfile.model: ModelConfig` inline, or `model_ref` → `~/.mur/models.yaml`) will not resolve on the recipient's machine. The wizard resolves it, **local-first but honest about tasks that need a cloud-class model**, driven by what the agent declares — not a global toggle.

### 7.1 `model_hint` (new manifest field)

Add an optional `model_hint` to the `.muragent` manifest (`mur-common/src/muragent/manifest.rs`), populated by the writer from the source agent's resolved binding:

```yaml
model_hint:
  provider: anthropic          # original provider
  name: claude-opus-4-7        # original model id
  tier: frontier               # small | mid | frontier  (derived via a model-class table)
  min_ram_gb: 0                # estimated RAM for a local equivalent (0 = cloud-class)
  local_capable: false         # was the agent authored against a local model?
```

- `local_capable` is the author's signal: bound to a local provider (ollama/mlx) → `true`; bound to a frontier cloud model → `false`.
- Derived at export time from a **model-class table** (a small static map in `mur-common`, versioned in-repo; unknown models fall back to `tier: mid, local_capable: true, min_ram_gb: conservative`).
- Optional + forward-compatible: validator treats absence as "no hint" (wizard then offers the neutral all-options menu).

### 7.2 Export sanitization change

Extend `sanitize_profile_for_export()`: convert `model_ref` → `model_hint` and **drop `model_ref`** (the referenced registry entry, which may hold an API-key secret-ref, never travels). Inline `model:` config is kept only for non-secret fields (provider/name/params); any secret material is stripped. This closes the model-side secret leak.

### 7.3 Resolution decision tree

On first run, detect recipient hardware (total RAM; Apple Silicon → MLX availability; Ollama presence) and read `model_hint`:

| Condition | Recommended (highlighted) default | Also offered |
|---|---|---|
| `local_capable` && hardware can run the tier | **Local** — pull via Ollama/MLX (the hinted model, or a mapped local equivalent), progress bar | paste API key; OpenAI-compatible endpoint |
| `!local_capable` (frontier) | **Cloud** — pick provider / paste key | "Use a strong local model instead (quality may drop)"; endpoint |
| `local_capable` && hardware too small | **Cloud, or a smaller local model**, with an explanation of the RAM gap | both local-small and cloud |
| no `model_hint` | neutral menu, no default pre-selected | all of the above |

Principles: never claim a small local model replaces a frontier one; the escape hatches (Ollama pull / MLX / paste key / endpoint) are **always** present regardless of the recommendation.

### 7.4 Applying the choice

- Write a recipient-side `~/.mur/models.yaml` entry (reuse `mur model add` / `ModelRegistry`) and set the agent's `model_ref` to it.
- Recipient-supplied API keys are stored in the recipient's own secret store (per `2026-05-20` §3.3 — mur never ships user secrets).
- Verify with a single test call before declaring success.
- Changeable later via `mur model` / Hub settings — not a one-way door.

### 7.5 Surfaces (GUI + CLI/non-interactive)

The wizard is **first-class on three surfaces**, sharing one resolution-logic module:

1. **Hub GUI** — a step folded into / following the §7.2 import dialog.
2. **CLI interactive** — `mur agent install` prompts in the terminal when no usable binding resolves.
3. **Non-interactive / scriptable** — for servers and the developer `--load` path (§8): `--model <ref>` flag and/or environment variables; fails fast with a clear message rather than blocking on a prompt when run headless.

---

## 8. Workstream D — Retire legacy export paths; toolchain-free developer path

- **`--format=gui`** → **hard-error** pointing to the Host + `.muragent` model (per `2026-05-20` §15). The 13-phase per-agent `.app` pipeline (`agent_export_gui.rs`, requiring cargo+node+npm+tauri) is retired as a *user* command; the Host itself is built once per release by mur's CI.
- **`doctor::checks_for`** drops the cargo/node/npm/tauri checks from the user-facing export path (they remain relevant only to the CI build of the Host).
- **`--format=bin`** (cargo build + raw-home tar) is **removed**, not patched — it required a toolchain and leaked the private key/secrets (`build.rs` tars the entire agent home). Both `--format=gui` and `--format=bin` return an **explicit redirecting error** (not the generic "unsupported format"): they name the replacement (`.muragent` + `mur-agent-runtime --load`). The toolchain-free, signing-intact developer/server replacement is:
  - **`mur-agent-runtime --load <path.muragent>`** — ship the pre-built (already-signed) runtime + run a sanitized `.muragent`. Reuses `installer`/`extract.rs` + the existing embedded-divert hook (`supervisor.rs:65`). The only new surface is a minimal arg parser in the runtime (`subcommand.rs` is currently empty; `main.rs` goes straight to `supervisor::entrypoint()`).
  - Idiomatic for its audience (`java -jar`, `docker run` style); preserves the runtime's signature because the binary is untouched and the payload is a data sidecar.
  - First-run model resolution uses the §7.5 CLI/non-interactive path.

The embedded single-file binary (append) remains the documented future option (§12), gated on a signing strategy.

---

## 9. Data-model & code impact

**`mur-common`**
- `muragent/manifest.rs`: add optional `model_hint` (§7.1).
- `muragent/writer.rs`: populate `model_hint` from profile binding.
- `muragent/validator.rs`: treat `model_hint` as optional/forward-compatible.
- new small **model-class table** module (tier / min_ram / local_capable lookup).

**`mur-core`**
- `cmd/agent/export.rs`: `sanitize_profile_for_export` converts `model_ref` → `model_hint` and strips model secrets (§7.2); remove `export_bin`; `--format=gui` and `--format=bin` → hard-error/redirect.
- `cmd/doctor.rs`: drop toolchain checks from the user path.
- `cmd/agent/install.rs`: CLI model-resolution prompts (§7.5 surface 2/3).

**`mur-agent-runtime`**
- `subcommand.rs` / `main.rs`: add `--load <path.muragent>` (and `--model <ref>` for non-interactive resolution).
- `export/bin_embed.rs`, `build.rs`: the `include_bytes!`/`MUR_EXPORT_AGENT_DIR` embed path is no longer driven by `--format=bin`; keep the runtime-side hook for the future append option but stop wiring the cargo build.

**`mur-hub-gui`**
- `src-tauri/tauri.conf.json`: fileAssociations, deep-link, `dmg`, signing/notarize (§5).
- `src-tauri/src/lib.rs`: open-file / open-url routing + single-instance (§5).
- `src-tauri/src/onboarding/*`: first-launch flow (§5).
- `src-tauri/src/export_muragent.rs` (new): Share command (§6).
- `src-tauri/src/import_muragent.rs`: add model-wizard commands (§7).

**`mur-gui-core`**
- `stub/*`: run the Gatekeeper validation gate (`2026-05-20` §12.4) when generating a launcher.

**Shared model-resolution module:** one logic core consumed by Hub GUI, CLI, and `--load` (§7.5). Location to be decided in the plan (candidate: `mur-gui-core` or a new `mur-core` module re-exported to the runtime).

---

## 10. Testing strategy

- **Unit:** `model_hint` derivation from each binding shape (inline ollama, inline frontier, `model_ref`); model-class table lookups incl. unknown-model fallback; sanitize drops `model_ref` + model secrets (property test: exported package contains no private key, no API key, no `model_ref`).
- **Resolution decision tree:** table-driven tests over (`tier` × `local_capable` × RAM × Ollama/MLX present) → expected recommended default + offered options. Pure function, no I/O.
- **Recipient import e2e:** produce a sanitized `.muragent` → import via installer → assert agent home, trust `pending`, model unbound → run wizard (mocked backends) → assert `~/.mur/models.yaml` entry + `model_ref` set + test-call invoked.
- **`--load` e2e:** pre-built runtime + sanitized `.muragent` → `--load` extracts + supervises; non-interactive `--model <ref>` resolves without prompting; headless with no binding fails fast with a clear message.
- **Host OS integration:** tauri.conf fileAssociations/deep-link present; open-file event routes to `inspect_muragent_file`; macOS Gatekeeper validation gate stays green for CI-signed Host (gated behind `MUR_APPLE_DEVELOPER_ID` like existing `agent_export_macos.rs`).
- **Regression:** removing `--format=bin`/`--format=gui` returns the hard-error, not a panic; legacy `.murpkg`/`.app` paths already covered by `2026-05-20` §11 (clean break, no migration).

---

## 11. Implementation order (preview; full plan via writing-plans)

1. **D-cleanup** — hard-error `--format=gui`/`--format=bin`; drop toolchain checks from `doctor` user path; remove `export_bin`. (Unblocks confusion, no new deps.)
2. **B-data** — `model_hint` manifest field + writer + validator + model-class table; sanitize `model_ref` → `model_hint`. Property tests.
3. **C** — Hub `export_muragent_file` Share command + UI (reuses #2 sanitize).
4. **runtime `--load`** — arg parser + extract/supervise + non-interactive `--model`.
5. **A** — `tauri.conf` (fileAssociations/deep-link/dmg/signing) + open routing + single-instance.
6. **B-wizard** — shared resolution module + Hub GUI step + CLI interactive prompts.
7. **A-onboarding** — first-launch `/Applications`/handler-registration/doctor flow.
8. **e2e** — recipient import + `--load` + Host OS integration suites.

---

## 12. Deferred / future

- **Embedded single-file binary (append).** Mechanically simple on all three OSes, but appending to a signed Mach-O/PE breaks the signature → recipient sees security warnings. A smooth zero-install single file needs per-export notarization (cloud signing service: cost/latency/network, contrary to local-first/free). Revisit only with a signing-service decision. The runtime-side embed hook is kept so this is additive.
- **Cross-platform export** (host on Mac, package for Windows). Additive: ship per-target pre-built runtimes / Host shells; the `.muragent` data and wizard are already platform-agnostic.
- **Share-by-URL** giveaway channel.
- **Commander cross-repo** consumption of `.muragent` (`2026-05-20` M-export-6).
- **V2 trust badge + `revocations.json`** issuer infrastructure.

---

## 13. Open questions / risks

1. **Host notarization in CI** — custody of mur's Developer ID + notarytool credentials in the release pipeline; Windows Trusted Signing onboarding.
2. **Model-class table maintenance** — who updates tiers/min-RAM as new models ship; unknown-model fallback must stay safe (prefer "offer all options" over a wrong confident default).
3. **Cross-platform RAM / accelerator detection** — reliable RAM read and Apple-Silicon/MLX detection across macOS/Windows/Linux.
4. **Deep-link reliability requires `/Applications`** (Tauri/LaunchServices constraint, `2026-05-20` §10.1) — onboarding must make this the default, not a buried option.
5. **Shared model-resolution module placement** — must be reachable from `mur-hub-gui`, `mur-core` CLI, and `mur-agent-runtime` without pulling GUI deps into the runtime.
6. **First-run latency** — a local pull of several GB is minutes on a fast link; the wizard must show real progress and allow "do this later / pick cloud now".
