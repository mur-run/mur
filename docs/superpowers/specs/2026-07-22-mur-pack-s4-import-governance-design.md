# MUR Pack S4 — Import Governance — Design

**Status:** Design / spec (S4 of the Pack governance program)
**Date:** 2026-07-22
**Builds on:** the Pack governance north-star (`2026-07-22-mur-pack-governance-design.md`, §6 import adapters), S1 (#742, never-shadow + de-pin), the existing Claude-plugin importer (`mur-core/src/cmd/agent/addon/{import,mod,parse,marketplace}.rs`), `AddonRef` (`mur-common/src/agent.rs:405`), and the skill provenance/hash model (`mur_common::skill::content_hash_for_origin`).

## 1. Goal

Bring the existing Claude-plugin importer under the Pack governance model — **without shadowing** builtin/registry skills, with a **pinned content hash**, and an explicit **re-import** refresh — the concrete gaps the current importer has. Lean: enhance the one importer we have; defer the general `ImportAdapter` trait until a second source exists (extracting an interface for one implementation is speculative structure).

## 2. Context — what the importer does and lacks

`mur agent addon import <agent> <source>` fetches a Claude plugin (local dir or `owner/repo` git shorthand → network install), security-scans it, and installs its `skills/` + `commands/` + `.mcp.json` into the agent as per-agent skill dirs + `McpServerEntry`s, recorded under one **fail-closed (disabled)** `AddonRef { id, source, enabled, skills, mcp, commands }`. Existing subcommands: `import`, `list`, `set_enabled` (enable/disable), `remove`, `disable_all`.

Gaps against the governance model:
- **Shadowing:** imported skills are written to the agent-local `skills/<name>/` dir with **no collision check** against builtin/global skills. This is exactly the S1 drift bug for imports — an imported skill named like a builtin silently shadows it via `load_all`.
- **No content-hash pin:** `AddonRef` records free-text `source` but no hash, so on-disk drift is undetectable and there is no clean "refresh from source."
- **No re-import:** there is no `reimport`; updating means `remove` + `import` by hand.

## 3. Decisions (settled during brainstorm)

| Question | Decision |
|---|---|
| Scope | **Import governance only** — enhance the existing importer; no `ImportAdapter` trait yet (one implementation). |
| Collision behavior | **Skip the colliding skill + warn**, import the rest. Not "refuse the whole import" (too harsh) and not "rename" (creates a divergent duplicate). The agent uses MUR's builtin/registry copy via store-wide injection. |
| Collision set | `mur_common::skill::local::list_installed(mur_home)` — the global store (builtins after sync + registry-installed). |
| Pin | `AddonRef.content_hash: Option<String>` — a stable hash over the imported skill+command manifests, recorded at import. |
| Refresh | New `mur agent addon reimport <agent> <id>` — re-fetch from `source`, re-scan + re-apply never-shadow, refresh the AddonRef + hash (consent-gated). |
| Trust | Unchanged: imported content installs **fail-closed disabled** (existing), low trust; `source` is the free-text provenance/TOFU record. No new trust store. |

## 4. Design

### 4.1 Never-shadow at import (`addon/import.rs`)
The importer builds a list of pending skills (`(dest_path, manifest)`) before writing them (import.rs ~182-320). Insert a governance gate over that list:
- For each pending skill, if its name is in `list_installed(mur_home)` (the global store), **do not write it**; emit a warning (`skill '<name>' is already provided by MUR — skipping the plugin's copy to avoid shadowing`), and exclude it from the `AddonRef.skills` list.
- Non-colliding skills install as today. Commands and MCP entries are unchanged (MCP already bails on a duplicate server name).
- Net effect: an imported plugin never overwrites or shadows a MUR-shipped skill; the agent keeps using the builtin/registry version.

### 4.2 `content_hash` pin (`mur-common/src/agent.rs` + `addon/import.rs`)
- Add `AddonRef.content_hash: Option<String>` (`#[serde(default, skip_serializing_if = "Option::is_none")]`, back-compat).
- Compute it at import as a deterministic hash over the installed skill + command manifests (e.g. sort by name, hash each via the existing skill content hash, fold into one SHA-256). Recorded on the `AddonRef`.
- `mur agent addon list` shows the short hash so the user can see the pin. (Drift detection — comparing on-disk content to the pin — can ride the existing `mur skill doctor` in a later pass; S4 only needs the pin recorded and refreshed.)

### 4.3 `reimport` (`addon/mod.rs` + CLI)
`mur agent addon reimport <agent> <id>`:
1. Look up the `AddonRef` by `id`; read its `source`.
2. Re-fetch from `source` (reuse the import path's network/local resolution), re-run the security scan and §4.1 never-shadow gate.
3. Replace the addon's on-disk skills/commands/MCP with the freshly-imported set, recompute `content_hash`, update the `AddonRef` in place. Preserve the `enabled` flag (a reimport of an enabled add-on stays enabled; a disabled one stays disabled).
4. Consent-gated like `import` (the existing confirm/scan flow).

### 4.4 Origin / TOFU
No change beyond §4.2: `AddonRef.source` already records where the add-on came from; imported content stays fail-closed disabled (existing), which IS the low-trust default. No new trust store — official/registry trust tiers are their own subsystems.

## 5. Out of scope / deferred
- The general `ImportAdapter` **trait** and additional adapters (generic pack URL, MCP registry) — extract when a second source is actually built (S5).
- **Store-level** never-shadow enforcement / reference-counted removal — S1's `mur skill doctor` already detects agent-local shadows post-hoc; §4.1 prevents imports from creating new ones.
- Signed/verified import provenance beyond the existing security scan.

## 6. Testing
- **Never-shadow:** importing a plugin whose bundle contains a skill named like a global-store skill skips that skill (not written to the agent dir; absent from `AddonRef.skills`) and warns, while non-colliding skills install; a plugin with no collisions installs unchanged.
- **content_hash:** an import records a non-empty `content_hash`; two imports of identical bundle content produce the same hash; a changed bundle produces a different hash; absent on a legacy `AddonRef` deserializes to `None`.
- **reimport:** reimport of an existing add-on re-applies from `source`, preserves the `enabled` flag, refreshes `content_hash`, and re-runs the never-shadow gate (a skill that became a builtin since the first import is now skipped); reimport of an unknown id errors.
- **Back-compat:** an `AddonRef` without `content_hash` round-trips; existing `import`/`list`/`remove`/`enable` behavior is unchanged for non-colliding bundles.
