# MUR Pack S3 — Capability Kind — Design

**Status:** Design / spec (S3 of the Pack governance program)
**Date:** 2026-07-22
**Builds on:** the Pack governance north-star (`2026-07-22-mur-pack-governance-design.md`, §3.3 the capability kind), S2 (#741, media skills are builtins), S1 (#742, de-shadow). Reuses shipped primitives: `McpServerEntry` (per-agent MCP wiring), `ProgramDep`/`requires_programs` (#684/#686 portable deps + `doctor`/`install-deps`), skill refs (builtin/registry), `Entitlements`.

## 1. Goal

Introduce **capability** as a coherent, standalone-installable tool bundle — MCP server(s) + skill refs + external-program requirements + suggested entitlements — that an agent can also declare as a dependency (`requires_capabilities`). Ship the **media** capability (VLC control + video analysis) as the first instance. CLI-only; no unified-manifest kernel, no runtime change, no `.muragent`/`.fleet` refactor (the brainstorm chose the lean path).

## 2. Context

A capability's constituent pieces already exist as fields on agents:
- `mcp_servers: Vec<McpServerEntry>` — per-agent MCP wiring (`command`, optional `binary_sha256`, `network`).
- `skills: Vec<String>` — skill refs; the media skills (`video-analyze`, `watch-together`, `scene-explain`, `vlc-control`) are **builtins** (inject store-wide — zero per-agent delivery cost).
- `requires_programs: Vec<ProgramDep>` — external programs, with cross-platform detect + `mur agent doctor` + `install-deps`.
- `entitlements: Entitlements { network, filesystem, processes }`.

So a capability is just a **named, reusable bundle of these** with an installer. There is no global-vs-agent-local shadow structure for MCP servers (unlike skills), so materializing a capability's MCP config into an agent profile does NOT reintroduce the S1 drift problem.

**Media capability's real requirements** (from `mur-mcp-server` + `mur-core::cmd::media`): it **spawns VLC** (`/Applications/VLC.app/...`, so **processes**) and controls it over **HTTP 127.0.0.1** (**network loopback**); it **reads local video files** (**filesystem read**); YouTube analysis shells out to **yt-dlp** (a second `ProgramDep`); scene/video analysis calls a **local model over loopback**.

## 3. Decisions (settled during brainstorm)

| Question | Decision |
|---|---|
| Scope | **Standalone capability**, lean — reuse existing primitives; no manifest kernel, no `.muragent`/`.fleet` refactor. |
| Delivery to an agent | **Install-into-agent (materialize)**: `mur capability install <name> --agent X` writes the MCP server entry + `requires_programs` + consented entitlements into X's profile. Skills come free (builtin). |
| Where the media capability lives | **Compiled into the binary** (`include_str!`), like builtin skills. No `~/.mur/capabilities/` global store yet (that is for third-party import — S4/S5). |
| Bidirectionality | An agent may declare `requires_capabilities: [media]`; standalone install is `install <name> --agent X`. |
| Uninstall | Remove the MCP servers this capability added (match by the capability's server names) + drop from `requires_capabilities`. **Conservatively keep** entitlements + `requires_programs` (may be shared). |

## 4. Design

### 4.1 `Capability` type (`mur-common/src/capability.rs`)
```rust
pub struct Capability {
    pub name: String,
    pub version: String,
    pub description: String,
    pub mcp_servers: Vec<McpServerEntry>,      // reused
    pub skills: Vec<String>,                    // refs (builtin/registry)
    pub requires_programs: Vec<ProgramDep>,     // reused
    pub entitlements: CapabilityEntitlements,   // suggested; requested at install
}
```
`CapabilityEntitlements` mirrors the requestable subset of `Entitlements` (network / filesystem read+write / processes). `capability.yaml` is its canonical serialization.

### 4.2 Builtin `media` capability (`mur-core/src/capabilities/media.yaml`)
Compiled in via `include_str!`. Contents:
- `mcp_servers`: one entry for `mur-mcp-server` (stdio) — `command` resolved to the `mur-mcp-server` binary basename, mirroring how other MCP servers are wired.
- `skills`: `[video-analyze, watch-together, scene-explain, vlc-control]` (already builtins).
- `requires_programs`: `VLC`, `yt-dlp` (each a `ProgramDep` with the existing detect/install metadata).
- `entitlements`: `processes` (spawn VLC/yt-dlp), `network` loopback (VLC HTTP + local model), `filesystem` read (video paths). Exact values pinned in the plan.

A `builtin_capabilities()` accessor returns the compiled-in set (currently just `media`), consumed by `list`/`show`/`install`.

### 4.3 `requires_capabilities: Vec<String>` on `AgentProfile`
New field, `#[serde(default)]` (empty when absent — back-compat). Records the capabilities an agent depends on. Populated by `install`, read by `list`/resolution.

### 4.4 `mur capability {list|show|install|remove}` (`mur-core/src/cmd/capability.rs`)
- **`list [--agent X]`** — builtin capabilities and, with `--agent`, which are installed on X (from its `requires_capabilities`).
- **`show <name>`** — the capability's contents (skills, MCP, programs, requested entitlements).
- **`install <name> --agent X`** — materialize into X:
  1. Upsert each `mcp_servers` entry into X's profile (by server name — idempotent; re-install upgrades).
  2. Skills: no-op for builtins (already inject store-wide); for any registry ref, ensure installed.
  3. Merge `requires_programs` into X and run the existing dependency check (report missing VLC/yt-dlp; `install-deps` path unchanged).
  4. Request the capability's entitlements via the **existing consent flow**; on consent, union them into X's `entitlements`.
  5. Add `<name>` to X's `requires_capabilities`.
  6. Save profile atomically; print that the agent must restart to apply (runtime loads the profile once).
- **`remove <name> --agent X`** — drop the MCP servers whose names match the capability's `mcp_servers`, remove `<name>` from `requires_capabilities`, save. Entitlements and `requires_programs` are **kept** (conservative — another capability or manual config may rely on them; documented in the command output).

### 4.5 Consent & safety
Install is a profile mutation gated by the existing entitlement-consent UX; nothing is granted silently. Materializing an MCP `command` does not fetch or execute a binary — it records the wiring; the program check (step 3) surfaces a missing VLC/yt-dlp rather than auto-installing. Agent restart required to take effect.

## 5. Out of scope / deferred
- **Global `~/.mur/capabilities/` store** + third-party capability **import/share** (peer/registry/catalog) → S4/S5.
- **Runtime-resolve reference** (agent declares `requires_capabilities`, runtime wires the MCP in-memory without materializing) → a follow-on if materialize-drift ever bites; MCP config rarely changes and the capability def is canonical.
- **Unified pack manifest / `kind` kernel** — still deferred; capability is its own focused format for now.
- Capability **versioning/upgrade** across a fleet; reference-counted entitlement/program removal.

## 6. Testing
- **Type round-trip**: a `capability.yaml` parses into `Capability` and back; the builtin `media` capability parses and names the four media skills + VLC/yt-dlp programs.
- **`requires_capabilities`**: absent → empty (back-compat); round-trips when present.
- **install**: on a temp agent, `install media` adds the `mur-mcp-server` `McpServerEntry`, merges VLC+yt-dlp into `requires_programs`, unions the entitlements, and appends `media` to `requires_capabilities`; a second `install` is idempotent (no duplicate server entry).
- **remove**: removes exactly the capability's MCP server entry and the `requires_capabilities` entry, and leaves entitlements + `requires_programs` intact.
- **consent**: install without consent to the entitlements does not mutate the profile (or mutates only the non-entitlement parts, per the existing consent flow's contract — pin in the plan).
- **list/show**: `list` reports `media` as a builtin; `show media` prints its contents.
