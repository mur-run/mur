# Fleet-Sync (Pro) — Design

**Date:** 2026-05-29
**Status:** Design / spec (implementation-ready after plan)
**Umbrella:** `docs/superpowers/specs/2026-05-29-mur-strategy-positioning-vs-archon.md` (§6 first paid point; §11 open questions on entity scope + conflict resolution)

Fleet-sync is MUR's **first paid point** (Pro, ~$10–20/mo). It replicates a user's *evolved* fleet across their own devices through the existing hosted server-sync path, preserving each entity's learning state (maturity / lifecycle / fitness) so that learning on machine A benefits machine B. This document closes the two design questions the umbrella strategy left open: which entities sync in v1, and how concurrent divergence is resolved.

---

## 1. Goal & non-goals

**Goal.** A Pro user signs in on a second device and recovers not just their agent *configuration* but their *evolved* agents — the same profiles, model bindings, skills, and workflows, with the accumulated maturity / fitness / lifecycle state intact — and ongoing changes on either device converge without losing learning.

**Non-goals (v1).**
- No cross-device replication of Ed25519 signing private keys (explicitly rejected — see §5).
- No plaintext secret sync (only secret-refs travel — see §5).
- No notes, companion appearance, or voice-model sync (deferred to v2 — see §3).
- No new sync protocol or storage engine; v1 extends the existing server-sync path (see §4).
- No dependency on the server-side draft *accept* endpoint (still TBD per umbrella §10).
- Out of scope: MUR Commander relay / SSH-tunnel transport (umbrella §10 stubs).

---

## 2. Background: existing sync (grounding)

The current hosted sync (in `mur-core/src/cmd/sync_cmd.rs`, types in `mur-common/src/sync_types.rs`) already covers **patterns** and **workflows**:

- Monotonic `.sync_version` counter per `~/.mur` workspace.
- A manifest mapping `name → { server_id, version, content_hash }` (`build_sync_changes`).
- Push carries `base_version` (`SyncPushRequest`); the server responds with a new `version` or `conflict: true` (`SyncPushResponse`).
- On conflict the client pulls the latest (`sync_pull_once`), rebuilds the change list, and retries.
- Auth via OAuth / device-code / API-key; transport is `POST /api/v1/...` against the Go `mur-server`.
- A separate git-based **device sync** path exists (`DeviceSyncDirection { Pull, Push, Both }`).

Fleet-sync reuses this versioned-blob + manifest + base_version-conflict machinery and extends it to agent-scoped entities.

---

## 3. v1 entity scope ("Evolved core")

Syncable entities in v1:

| Entity | Source on disk | Notes |
|---|---|---|
| Agent profile | `~/.mur/agents/<slug>/profile.yaml` (`AgentProfile`) | Minus signing private key (§5). System prompt file (`sys_prompt_file`) travels with it. |
| Model bindings | `model_ref` in profile + `~/.mur/models.yaml` entries | Secret values stripped to refs (§5). |
| Installed skills | `AgentProfile.installed_skills` + skill content under `~/.mur` | Logical skill identity + version. |
| Workflows | `~/.mur/workflows/*.yaml` | Already partly synced today; folded into the fleet unit. |
| Pattern evolution state | `~/.mur/patterns/*.yaml` maturity / lifecycle / fitness / decay fields | The point of the feature — see §6 merge classes. |

**Deferred to v2:** notes, companion `appearance`, voice models/config. These add conflict surface without changing the core "evolved fleet" value proposition.

Each entity type carries its **own** monotonic version counter and manifest file under `~/.mur/.fleet-sync/` (per-entity-type counters, not a single unified fleet version vector — simpler for v1; a unified vector is a possible v2 refinement).

---

## 4. Substrate: extend hosted server-sync

The Go `mur-server` treats every fleet entity as an **opaque, versioned blob** keyed by `(owner, entity_type, logical_id)` with a `version` and `content_hash`. The server does **not** parse entity schemas and does **not** perform field-level merges — it only enforces the optimistic-concurrency `base_version` check and stores/returns blobs. This keeps all schema-aware merge logic in Rust (`mur-common`) and leaves the server unchanged in its conflict semantics.

New endpoints mirror the pattern path, e.g.:

- `GET  /api/v1/core/fleet/<entity_type>?since=<base_version>` → list of `{ logical_id, version, content_hash, payload }`.
- `POST /api/v1/core/fleet/<entity_type>` → `{ base_version, changes: [...] }`; responds `{ version }` or `{ conflict: true }`.

Reuses existing auth (OAuth / device-code / API-key). Access is gated behind the **Pro entitlement** (existing LemonSqueezy Free/Pro/Team tiers); the CLI checks entitlement before any fleet push/pull.

New shared types live in `mur-common/src/sync_types.rs` alongside the pattern types (`FleetEntityType`, `FleetChange`, `FleetPushRequest`/`Response`, `FleetPullResponse`), following the existing naming.

---

## 5. Identity & secrets (ref-only + per-device key)

- **Logical identity syncs; signing key does not.** The agent's stable logical identity (`AgentProfile.id` UUIDv7, `name`, `identity.owner`) is replicated. The Ed25519 *signing private key* (`identity.key`) is **not** synced — each device generates and holds its own key under the shared `owner`. Cross-host A2A continues to work because peers trust by `owner` + per-device pubkey, not by a single shared private key. (Rationale: replicating a private key across devices enlarges the key-exposure surface for marginal UX gain.)
- **Profile sync strips the key.** The synced profile blob omits `identity.pubkey`/private material; on pull, a device that lacks a local key for that agent generates one (key_version 0) and fills its own `identity` block, leaving the rest of the synced profile intact.
- **Secrets travel as refs only.** `model_ref` bindings and any credentialed config sync the **secret-ref** (pointer into `models.yaml` / OS keychain per the model-registry + secret-refs design), never plaintext. Each device resolves the ref from its local keychain.
- **Missing secret → degraded, not broken.** If a pulled binding's secret-ref does not resolve locally, sync reports it (`mur sync status`) and the agent loads in a **degraded / unbound** state until the user supplies the secret on that device. Sync itself still succeeds.

---

## 6. Conflict resolution: field-aware merge (client-side)

When two devices mutate the same entity, the **Rust client** performs a field-aware merge on conflict (server stays a dumb versioned-blob store). Every syncable struct declares a **merge-policy table** classifying each field:

**Class A — cumulative / monotonic (never lose learning):**
- Fitness counters, `usage_count`, co-occurrence tallies → **sum of deltas** (merge by combining each side's increment over the common base) or **max** where only a running total exists.
- `maturity` (Draft → Emerging → Stable → Canonical) → **max** (only ever advances).
- `decay` last-touched / last-seen timestamps → **max** (most-recent wins, monotonic).
- Evidence / links collections → **set union** (dedup by stable key).

**Class B — descriptive / replaceable:**
- `persona`, system prompt, `model_ref` binding, `display_name`, `description`, `skills` list → **version-vector + last-writer-wins**, with `--force-local` override as the escape hatch (consistent with today's `ForceLocal`).

**Merge flow (per entity, on `conflict: true`):**
1. Client pulls the server blob (its `version` + payload).
2. Client computes the common base from its manifest and applies the per-field merge policy: Class A fields combined, Class B fields resolved by version-vector/LWW.
3. Client writes the merged entity locally, then re-pushes with the new `base_version`.
4. Retry loop bounded (same shape as the current pattern push retry); on repeated conflict the client surfaces a clear message and `--force-local` is available.

The merge-policy table and the merge function live in `mur-common` so they are unit-testable independently of network I/O.

---

## 7. CLI surface

- `mur sync fleet [--pull | --push | --both] [--force-local]` — mirrors `DeviceSyncDirection`; default `--both`. Checks Pro entitlement first.
- `mur sync status` — per-entity-type drift summary (local vs server version), plus a list of bindings whose secret-ref does not resolve on this device (degraded agents).
- Existing `mur sync` pattern/workflow behavior is unchanged; fleet sync is additive.

---

## 8. Data flow (summary)

**Push.** For each entity type: scan local source dirs → build change list by comparing `content_hash` against the per-type manifest → `POST` with `base_version` → on `{ version }` update manifest; on `{ conflict: true }` run the §6 merge flow and retry.

**Pull.** For each entity type: `GET ?since=<base_version>` → for each returned entity, apply §6 field-aware merge into the local profile / skill / workflow / pattern state → update manifest and per-type version.

**Entitlement gate.** Both directions short-circuit with a clear upgrade message if the account lacks the Pro entitlement.

---

## 9. Testing

- **Merge-policy unit tests** per field class: Class A `max` / `union` / `sum-of-deltas`; Class B version-vector LWW; `--force-local` override.
- **Two-device round-trip:** simulate two manifests diverging (A bumps fitness, B edits persona) → converge → assert fitness summed and persona resolved by LWW, no learning lost.
- **Secret-ref-missing degraded mode:** pull a binding whose secret does not resolve locally → sync succeeds, `mur sync status` flags it, agent loads unbound.
- **Identity isolation:** pulled profile never carries a private key; a fresh device generates its own key_version 0 while preserving the synced logical id.
- **Back-compat:** legacy `AgentProfile` / patterns without fitness/lifecycle blocks sync cleanly (serde defaults).
- **Entitlement gate:** non-Pro account is refused before any network write.

---

## 10. Sequencing & honesty caveats

- Independent of the TBD server-side draft *accept* endpoint (umbrella §10); fleet-sync uses its own fleet endpoints.
- Commander relay / SSH-tunnel transport remains out of scope.
- Reconcile the stale `mur-commander` version note (umbrella §10) is **not** part of this work.
- v1 ships per-entity-type version counters; a unified fleet version vector is a candidate v2 refinement.

---

## 11. Open questions (for the plan)

- Exact wire shape of `FleetChange` payloads (full blob vs delta) — lean toward full-blob v1 for simplicity given small entity sizes.
- Whether installed-skill *content* (vs identity + version) needs to travel, or whether each device re-fetches skill content from its source — decide during planning.
- Manifest storage location/format under `~/.mur/.fleet-sync/` and migration from any existing `.sync_version`.
