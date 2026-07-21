# MUR Pack Governance — Agent / Fleet / Capability Export & Import — Design

**Status:** Design / spec (architecture north-star for a multi-phase program)
**Date:** 2026-07-22
**Builds on:** `.muragent` writer/validator + export sanitization (#740), `.fleet` bundle export/import (signed tar.gz), Claude-plugin import (`cmd/agent/addon/import.rs`), quill P2.1 publisher keyring + P2.2 registry signing, #686 trusted-publisher install recipes, the shipped skill provenance+upgrade model (`skill.origin`/`origin_version`/`origin_hash`, `cmd/skill_upgrade.rs`), the official catalog client+server (PR #738, mur#739, mur-server#31): per-item account-bound `OfficialLicense`, subscription-gated pro tier.

## 1. Goal

Unify MUR's fragmented distribution formats (`.muragent`, `.fleet`, Claude-plugin import, official catalog) into ONE governed **Pack** model that supports: multi-component packs (skills + MCP bundled coherently), fleets composed of independently-installable agents, a first-class **capability** kind (MCP + skills + entitlements), and governed external import — all sharing one provenance, trust, upgrade, and entitlement core.

## 2. Decisions (settled during brainstorm)

| Question | Decision |
|---|---|
| Top-level unit | **Agent = atomic installable/sellable**; **Fleet = orchestration pack that REFERENCES member agents** (fleet-specific agents embedded); **Capability = new kind** (MCP + skills + entitlements) |
| Format | **Unified manifest + `kind: agent\|fleet\|capability`**, shared crate for refs/provenance/entitlement/signing/upgrade/import. `.muragent`/`.fleet` retained as wrappers (each is a Pack kind) |
| Component delivery | **Reference + content-hash pin**, not embed. Vendor (embed) ONLY pack-specific components. Skills → builtin/registry refs; MCP → recipe refs. Never embed MCP binaries |
| Never-shadow | One **single ownership channel** per component. Referenced/imported content must NEVER shadow builtin/official (the drift bug found in the current concierge) |
| MCP delivery | **Recipe reference** via #686 trusted-publisher (sha256, cross-platform detect, install-at-import). No embedded binaries |
| Provenance & upgrade | **Generalize the shipped skill model**: every installed unit carries `origin` + `origin_version` + `origin_hash`; `mur upgrade` upgrades-if-unmodified, flags-if-modified, never clobbers local edits |
| Capability composition | **Bidirectional**: capability is standalone-installable (`install capability/media`) AND an agent may declare `requires_capabilities: [media]` (resolved on agent install) |
| Trust | Official (2-key: publisher + license) / Registry (quill DSSE + keyring) / Peer-TOFU (shares, plugins, third-party). Pack signature covers its manifest; each referenced component verified at its own tier |
| Import | Unified adapters (Claude-plugin = one adapter); imported content lands in its OWN channel, TOFU trust, pinned `origin`, never shadows |
| Business | **Subscription unlocks the whole pro catalog** (already built); per-item account-bound license (anti-share). Fleet effective tier = `max(members)` — all-or-nothing |
| Uninstall | **Reference-counted**: remove only components exclusive to the uninstalled unit; keep shared; builtin never removed |

## 3. Architecture — three pillars

### 3.1 Unified Pack kernel
A shared crate (`mur-common::pack`) owns the manifest schema and the cross-cutting logic. The manifest is `kind`-tagged:

```yaml
schema: mur-pack/1
kind: agent            # agent | fleet | capability
name: researcher
version: 1.0.0
# common blocks (present per kind):
components:            # references, not embeds (see 3.2)
  skills:      [{ name, source, version, content_hash }]
  mcp:         [{ command_basename, recipe_ref, requires_programs, content_hash }]
  capabilities: [media]        # agent-declared dependencies
  agents:      [{ id, version, content_hash }]   # fleet members
entitlements: { network, filesystem, processes }  # requested; recipient consents
provenance: { origin, origin_version, origin_hash }
signature: { … }        # over the manifest (DSSE-style), verified per trust tier
```

`.muragent` and `.fleet` remain the on-disk wrappers; the writer/validator/installer become thin per-kind adapters over the shared kernel. New kinds (capability) add a `kind` value + a small adapter, not a new format.

### 3.2 Components: reference, don't embed
Every dependency is a **reference** carrying `{name, source, version, content_hash}`:
- **source = builtin** → resolved from the ship-with-binary set (`sync_cmd.rs`), owned by the MUR release.
- **source = registry** → resolved from quill (DSSE + keyring verified), owned by the registry publisher.
- **source = catalog** → resolved from the official catalog (agents/fleets/capabilities), owned by official, subscription-gated.
- **source = embedded** → the content travels in the pack; used ONLY for pack-specific components with no upstream (a fleet-internal worker, a bespoke skill).

**Resolution on install = reference-with-embed-fallback**: try to resolve from the declared source (verify `content_hash`); fall back to the embedded copy only if unresolvable. This kills gratuitous vendoring: a component already present as builtin/registry is never re-written locally, so it can never shadow its upstream.

**Never-shadow (single ownership channel):** each component name has exactly one owning channel. `load_all`'s agent-local-shadows-global behavior is retained ONLY for genuinely pack-specific vendored components; a vendored copy whose `content_hash` equals the builtin/registry copy is dropped at install (no shadow, no drift).

### 3.3 The three kinds
- **Agent** — atomic, independently installable & sellable. References skills + MCP + `requires_capabilities`; embeds only pack-specific content. Standalone-useful.
- **Fleet** — orchestration over agents: references member agents (each a catalog item, independently installable) + topology (router, shared channel, dependency DAG) + optionally embeds fleet-specific agents that have no standalone use. Installing a fleet resolves/installs its member agents (subscription-gated per §5) plus the orchestration config.
- **Capability** — a coherent tool bundle: MCP recipe ref(s) + the skills that use them (refs) + `requires_programs` (e.g. VLC) + suggested entitlements. Standalone-installable and agent-declarable-as-dependency. The media capability (`mur-mcp-server` + video-analyze/watch-together/scene-explain/vlc-control + VLC) is the first instance.

## 4. Provenance & upgrade (generalize the shipped skill model)
The skill layer already ships the correct model (`cmd/skill_upgrade.rs`): compare upstream `latest` vs local `origin_version`; if the local `content_hash != origin_hash` the unit is *modified* and is reported, never overwritten; otherwise upgrade and re-stamp `origin_version`/`origin_hash`. This design **lifts that exact mechanism into the pack kernel** so it applies uniformly to skills (today), agents, fleets, and capabilities. `mur upgrade [--check]` walks every installed unit with an `origin` and reports/applies per the same modified-safe rule.

## 5. Trust, entitlement & business
- **Trust tiers**: official (publisher+license 2-key, catalog) / registry (quill DSSE) / peer-TOFU (`.muragent`/`.fleet` shares, imported plugins, third-party packs). A pack's signature authenticates its manifest; each referenced component is verified at ITS source's tier on resolution. Imported/peer content installs at lowest trust, pinned, never auto-promoted.
- **Entitlements**: a pack declares requested `network/filesystem/processes` (and capabilities their MCP tools need); the recipient consents at install (existing muragent/fleet consent flow). Machine-specific grants are stripped on export (#740).
- **Business**: subscription unlocks the whole pro catalog (built). Per-item license stays account-bound (anti-share). **Fleet effective tier = max(member tiers)** — a fleet with any pro member is a pro item; a non-subscriber is blocked at `/download` (no partial install).

## 6. Import / external plugins
A unified **import adapter** interface normalizes an external source into the Pack model while preserving provenance. The existing Claude-plugin importer (`addon/import.rs`, plugin → per-agent skills + command-skills + MCP under an `AddonRef`) becomes the first adapter. Imported content: lands in its own channel, `origin` = source URL/plugin id, TOFU low trust, pinned `content_hash`; updated only by explicit re-import from source; **never shadows** builtin/official. Additional adapters (generic pack URL, MCP registry) share the interface.

## 7. Lifecycle
- **Install**: resolve refs (verify hashes) → consent to entitlements → materialize. `requires_capabilities` prompts to install missing capabilities.
- **Upgrade**: §4 (modified-safe, per-unit `origin`).
- **Uninstall**: reference-counted — remove only components exclusive to the removed unit; keep shared; builtin never removed.

## 8. Implementation decomposition (this is a program)
1. **S2 — near-term shadow cleanup (FIRST, this cycle):** promote `mur-native-tools` into the builtin set (`sync_cmd.rs`); de-pin the 4 concierge skills that shadow builtin (`mur-compress`, `parallel-code`, `video-analyze`, `watch-together`); add a `mur skill doctor` shadow-drift check (local copy's content differs from a same-name builtin/registry copy → warn). Validates the never-shadow principle with a small, immediately-useful change. Keep `concierge` (identity) and `brainstorming` (registry-owned, stays registry).
2. **S1 — kernel:** extract the unified Pack manifest + `kind` + shared crate from `.muragent`/`.fleet`; lift `origin`/upgrade into the kernel; generalize to agents.
3. **S3 — capability kind:** MCP recipe + skills refs + entitlements; ship the media capability as the first instance.
4. **S4 — import adapters:** the adapter interface + imported-channel governance; refit `addon/import` as the first adapter.
5. **S5 — distribution:** catalog publish for fleets + capabilities (agent path largely built).

Each sub-project gets its own plan (and spec if warranted) at execution time.

## 9. Out of scope / future
- KMS/HSM for signing keys; rollback/downgrade protection; revocation lists (deferred, as in the catalog specs).
- OCI / git-native pack sources beyond the first adapters.
- Team-seat sharing of entitlements.

## 10. Testing approach
- Kernel: round-trip a `kind`-tagged manifest per kind; reference resolution (builtin/registry/catalog/embedded) with hash verification; never-shadow (a vendored copy equal to builtin is dropped; a different one is flagged, not silently shadowing).
- Provenance/upgrade: unmodified upgrades + re-stamps; modified is flagged not clobbered (mirror the existing skill_upgrade tests) — per kind.
- Capability: install resolves MCP recipe + skills + entitlement consent; agent `requires_capabilities` pulls the capability.
- Entitlement/business: fleet tier = max(members); non-subscriber blocked on a pro-member fleet.
- Uninstall: reference-counted removal keeps shared deps, drops exclusive, never builtin.
