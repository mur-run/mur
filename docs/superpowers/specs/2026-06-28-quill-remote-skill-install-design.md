# Quill — Install skills from a remote URL / registry (Hub + CLI)

**Status:** Design / spec
**Date:** 2026-06-28
**Codename:** quill (the skills sibling of **feather**, which does the same for MCP servers)
**Scope:** Let a user install a skill onto an agent from a remote URL (P1) or a skill registry (P2), reusing MUR's existing schema-validation + security-scan + install path. Skills are static content, so this is always download-and-install (no "connect" model).

---

## 1. Problem

Today a skill can only be installed from a **local path**:
- CLI: `mur agent skill add <agent> <source>` (`cmd_skill_add`, `source` = a local `skill.yaml` or `.md`).
- Hub: `agent_skill_install(name, source_path)` Tauri command, driven by the "Install skill…" button on the agent Skills tab.

There is no way to install a skill from a URL or a registry. feather just added remote MCP servers by URL; quill is the same affordance for skills.

## 2. Key insight — the security infrastructure already exists

`cmd_skill_add` already validates the skill schema and runs `mur_common::skill::scan::scan_skill(&manifest)`, which includes:
- **`scan_executable`** — flags `curl … | sh|bash|python|…` and similar (the `curl|sh` supply-chain vector).
- **`scan_injection`** — flags prompt-injection patterns ("ignore all previous instructions", embedded `<system>…</system>`, "send your api_key to …").

So remote skill install is mostly: **download the skill from a URL → hand it to the existing scan+install path → show the skill body + scan findings for consent.** The scan is the security gate; the consent screen is the human review of the prompt-injection surface (analogous to feather's tool-description consent for tool poisoning).

## 3. Distribution models & ranking

Skills are declarative text (`skill.yaml`/`.md`) that must live locally to be injected — so unlike feather there is no remote "connect" model; every model is download-and-install. The chosen scope:

| Model | URL is… | Status |
|---|---|---|
| **A — raw skill file** | `https://…/skill.yaml` or `…/skill.md` | **P1** |
| **C — skill registry** | a catalog; install by name | **P2** (built on A) |
| B — skill bundle (`.tar.gz`/assets) | out of scope (revisit if multi-file skills appear) |

## 4. What MUR already has (reuse map)

| Capability | Existing primitive | Location |
|---|---|---|
| Install a skill from a path (schema-validate + scan + register) | `cmd_skill_add(agent, source)` | `mur-core/src/cmd/agent/skill.rs:49` |
| Security scan (executable + injection) | `scan_skill`, `scan_executable`, `scan_injection` | `mur-common/src/skill/scan/{mod,executable,injection}.rs` |
| Hub install Tauri command (+ refreshed detail + installed id) | `agent_skill_install(name, source_path) -> SkillInstallResult` | `mur-hub-gui/src-tauri/src/mcp_skills.rs` |
| Hub Skills tab UI ("Install skill…", loadable/dead states) | DetailPanel skills section | `mur-hub-gui/ui/src/components/DetailPanel.tsx` |
| HTTPS client + URL validation pattern | `reqwest`; feather's `mcp_remote::validate_remote_url` | `mur-core` |
| `.md` → `skill.yaml` conversion on install | `cmd_skill_add` | `skill.rs` |

**Conclusion:** P1 is "download to a temp file → call `cmd_skill_add` → surface manifest + scan findings." P2 adds a catalog client that resolves to a file URL and reuses P1. No new on-disk skill format.

## 5. Design — phased

### Phase 1 (P1) — install a skill by URL

**Shared core (mur-core, new `skill_remote.rs`):**
- `validate_skill_url(url) -> Result<String>` — require `https` (localhost-http exception), normalize. (Mirror `mcp_remote::validate_remote_url`; consider extracting the shared validator.)
- `fetch_skill(url) -> Result<FetchedSkill>` — async reqwest GET, **size-capped** (e.g. 1 MiB; constant, not a literal scattered around), writes the body to a temp file whose extension is derived from the URL/content-type (`.yaml` vs `.md`). Returns `{ temp_path, suggested_name }`.
- `preview_skill(temp_path) -> Result<SkillPreview>` — parse the manifest (name/description/category/body) and run `scan_skill`, returning `{ manifest summary, body, findings: Vec<ScanFinding> }` WITHOUT installing. Powers the consent screen.
- `install_skill_from_url(agent, url, accept_findings: bool) -> Result<...>` — validate → fetch → preview; if `findings` non-empty and `!accept_findings`, **refuse** (fail-closed); else call the existing `cmd_skill_add(agent, temp_path)` (which re-scans + installs) and clean up the temp file.

**CLI:** `mur agent skill add-url <agent> <url> [--yes]` (and/or `skill add` auto-detects an `http(s)://` source). `--yes` accepts scan findings non-interactively; default prompts/refuses.

**Hub:** new Tauri commands wrapping the above:
- `agent_skill_preview_url(url) -> SkillPreview` (fetch + parse + scan; installs nothing).
- `agent_skill_install_url(name, url, acceptFindings) -> SkillInstallResult`.
UI: an "Install from URL" button next to "Install skill…" on the Skills tab → modal: URL → **Fetch & review** → consent screen rendering the skill **name + description + full body** with scan findings highlighted (injection/executable shown prominently) → **Install** (disabled until a successful preview; if findings exist, an explicit "I understand, install anyway" checkbox gates it). Per-agent; "restart agent to load it" hint (matches existing copy).

### Phase 2 (P2) — skill registry (built on P1)

- A skill registry = a JSON catalog fetched from a **configurable index URL** (config key, default a MUR-hosted/curated index; no hardcoded literal per CLAUDE.md rule 1). Catalog entry: `{ name, description, publisher, file_url, version? }`.
- mur-core: `skill_registry::{search(query), resolve(name) -> file_url}` (fetch + filter the index).
- CLI: `mur agent skill search <query>`, `mur agent skill registry-add <agent> <name>`.
- Hub: "Browse registry" → search list (name + description + publisher) → Install → resolves `file_url` → **reuses P1's fetch+preview+consent+install**.
- The hosted index endpoint (app.mur.run) is a **separate server-side task**; P2 ships the client + the configurable default index URL so it works against any conforming index.

## 6. Data model

No new on-disk skill format. P1 adds no persisted fields (skills install exactly as today). Internal types only: `FetchedSkill`, `SkillPreview { name, description, category, body, findings }`, and (P2) `RegistryEntry { name, description, publisher, file_url }`. If `SkillManifest` lacks a `source_url`/publisher provenance field, optionally record the origin URL in `SkillStats`/manifest metadata for audit (nice-to-have, not required for P1).

## 7. Security model

- **Scan is the gate.** `scan_skill` runs on preview AND again inside `cmd_skill_add` at install. Findings (injection/executable) block install unless the user explicitly accepts (fail-closed default).
- **Full-body consent.** The consent screen shows the entire skill body — the prompt-injection surface — not just name/description.
- **HTTPS only** (localhost-http exception for dev); size-capped download; download performed by MUR's trusted control plane (CLI/Hub), not inside a sandboxed agent.
- **Provenance** (P2): show the registry publisher; record the origin URL for audit.
- Aligns with MUR's standing posture: opt-in, fail-closed, reviewed before trust.

## 8. Error handling

- Invalid/non-https URL → rejected before fetch.
- Fetch failure (DNS/TLS/timeout/too-large) → surfaced inline; nothing installed.
- Not a valid skill (schema parse fails) → clear error; nothing installed; temp file cleaned up.
- Scan findings + no acceptance → refuse with the findings listed.
- Duplicate skill id → reuse `cmd_skill_add`'s existing behavior (overwrite/skip as it does today).
- Changes apply on agent **restart** (existing model; surface in copy).

## 9. Testing

- mur-core unit tests: URL validation; extension/name derivation; `preview_skill` returns findings for an injection/`curl|sh` sample and clean for a benign skill; `install_skill_from_url` refuses on findings without `accept`, installs with it (using a `file://`/temp fixture to stay network-free); size-cap enforcement.
- A gated `#[ignore]` network test against a real raw skill URL (e.g. a GitHub raw `skill.md`) asserting preview returns the manifest.
- Hub: Tauri command wrappers return typed results; modal state machine (fetch → review → install) with mocked invoke.
- Manual/live: install a clean skill by URL end-to-end; install one with an injection line and confirm it's blocked pending acknowledgment.

## 10. Non-goals

- Skill bundles / multi-file skills (model B).
- Building the hosted registry **backend** (app.mur.run endpoint) — client + configurable index only in P2.
- Auto-update of installed skills (re-install manually).
- Changing the local "Install skill…" path beyond sharing the new consent screen.

## 11. Open questions

- Should `skill add` auto-detect URLs, or keep a distinct `add-url` subcommand? (Lean: distinct subcommand for clarity; auto-detect is a convenience add.)
- Record origin URL + publisher into the installed skill for audit now (P1) or defer to P2? (Lean: cheap to record in P1.)
- Default registry index URL + JSON schema for P2 — finalize when the server-side index task is scoped.
