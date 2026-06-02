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

> **Data-model note (2026-05-29).** This scope targets the **post-migration** model
> from `2026-05-28-mur-notes-design.md` + Workflow Engine v2, where the `Pattern`
> type and `~/.mur/patterns/` are **removed** and every knowledge object becomes a
> `Skill` under `~/.mur/skills/<name>/` (a Note is `category: note`, a Workflow is
> `category: workflow`). Evolved state lives in per-skill `stats.yaml` (`SkillStats`:
> lifecycle / usage / usefulness) + append-only `events.jsonl`, **not** in the
> manifest. Fleet-sync deliberately does **not** sync legacy `~/.mur/patterns/`
> (being deleted). See the dependency in §10.

Syncable entities in v1:

| Entity | Source on disk | Notes |
|---|---|---|
| Agent profile | `~/.mur/agents/<slug>/profile.yaml` (`AgentProfile`) | Minus signing private key (§5). System prompt file (`sys_prompt_file`) travels with it. Model bindings are the profile's `model_ref` + referenced `~/.mur/models.yaml` entries, secrets stripped to refs (§5). |
| Skill corpus (unified) | `~/.mur/skills/<name>/` — all categories (`note`, `workflow`, `context`, `command`, `meta`) | Each skill dir = `skill.yaml` (signed manifest, `content_sha256`) + `stats.yaml` (evolved state, derived) + `events.jsonl` (append-only) [+ `runs.jsonl` for workflows]. Subsumes what were previously separate "installed skills", "workflows", and "pattern evolution state" — they are all skills now. |

**Deferred to v2:** notes-as-distinct-tree (no longer applicable — notes are skills), companion `appearance`, voice models/config. These add conflict surface without changing the core "evolved fleet" value proposition.

**Out of scope (legacy):** `~/.mur/patterns/*.yaml`. Patterns are being removed by the Workflow-Engine-v2 / Notes migration (no automatic migration; users curate exported patterns into notes). Fleet-sync syncs the skill corpus, not the legacy pattern tree.

Each entity type carries its **own** monotonic version counter and manifest file under `~/.mur/.fleet-sync/` (per-entity-type counters, not a single unified fleet version vector — simpler for v1; a unified vector is a possible v2 refinement). For the skill corpus, the manifest is keyed by skill `name` and tracks the `skill.yaml` `content_sha256` plus an `events.jsonl` tail marker (§6).

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

## 6. Conflict resolution: event-union + signed-manifest LWW

The post-migration skill model makes conflict resolution simpler and more robust
than generic field-level merge, because the evolved state is **derived from an
append-only log**. The Rust client resolves conflicts (the server stays a dumb
versioned-blob store) using two mechanisms, matched to the two on-disk shapes:

**A. Evolved state — `events.jsonl` set-union + deterministic re-reduce.**
Lifecycle / usage / usefulness state is **not** stored as mergeable scalar fields;
it lives in append-only `events.jsonl` (retrieval/run/`superseded`/`dismissed`
events) and is reduced into `stats.yaml` by the shared reducer
(`skill/stats.rs`, `skill/aggregator.rs`). Therefore:
- Merge = **set-union of event lines** across devices, deduped by a stable event key
  (`ts` + `kind` + `outcome` + source-device, or an explicit event id if present).
- After union, **re-run the shared reducer** to recompute `stats.yaml`
  deterministically. `stats.yaml` is a *derivative* (like the LanceDB index) and is
  **never merged directly** — it is always rebuildable from the unioned log.
- This is naturally conflict-free for cumulative learning: two devices' usage
  histories combine; no `max`/`sum` field arithmetic, no lost learning. `runs.jsonl`
  (workflows) merges the same way.

**B. Authored manifest — `skill.yaml` as a signed opaque blob, version-vector + LWW.**
The manifest is DSSE-signed over its canonical YAML (`content_sha256` underpins
signing, drift detection, trust hash, registry lookup). It therefore **cannot be
field-merged** without breaking the signature. Sync it as a whole **opaque signed
unit** resolved by version-vector + last-writer-wins, with `--force-local` as the
escape hatch (consistent with today's `ForceLocal`). The same applies to the
`AgentProfile` blob (minus identity key, §5).

**Merge flow (per entity, on `conflict: true`):**
1. Client pulls the server blob (its `version` + payload).
2. **Skill corpus:** union the local and remote `events.jsonl` (dedup), re-reduce to
   regenerate `stats.yaml`; for `skill.yaml`/profile, resolve the signed manifest by
   version-vector/LWW (`--force-local` override available).
3. Client writes the merged result locally, then re-pushes with the new `base_version`.
4. Retry loop bounded (same shape as the current pattern push retry); on repeated
   conflict the client surfaces a clear message and `--force-local` is available.

The event key, dedup, and union+reduce logic live in `mur-common` (reusing the
existing reducer) so they are unit-testable independently of network I/O.

---

## 7. CLI surface

- `mur sync fleet [--pull | --push | --both] [--force-local]` — mirrors `DeviceSyncDirection`; default `--both`. Checks Pro entitlement first.
- `mur sync status` — per-entity-type drift summary (local vs server version), plus a list of bindings whose secret-ref does not resolve on this device (degraded agents).
- Existing `mur sync` pattern/workflow behavior is unchanged; fleet sync is additive.

---

## 8. Data flow (summary)

**Push.** For each entity type: scan local source dirs → build change list by comparing `content_hash` against the per-type manifest → `POST` with `base_version` → on `{ version }` update manifest; on `{ conflict: true }` run the §6 merge flow and retry.

**Pull.** For each entity type: `GET ?since=<base_version>` → for each returned entity, apply §6 resolution (skill corpus: event-union + re-reduce; `skill.yaml`/profile: signed-blob LWW) into the local skill dirs / profile → update manifest and per-type version.

**Entitlement gate.** Both directions short-circuit with a clear upgrade message if the account lacks the Pro entitlement.

---

## 9. Testing

- **Event-union merge unit tests:** union of two `events.jsonl` logs dedups by event key, and re-reducing the union yields the same `stats.yaml` regardless of merge order (commutative/idempotent).
- **Signed-manifest LWW:** `skill.yaml` / profile resolved by version-vector LWW; `--force-local` override; merged manifest's `content_sha256` and signature stay valid (whole-blob replacement, never field-edited).
- **Two-device round-trip:** device A logs usage on a skill, device B edits the same skill's manifest → converge → assert event histories combined (no lost usage) and manifest resolved by LWW.
- **Secret-ref-missing degraded mode:** pull a binding whose secret does not resolve locally → sync succeeds, `mur sync status` flags it, agent loads unbound.
- **Identity isolation:** pulled profile never carries a private key; a fresh device generates its own key_version 0 while preserving the synced logical id.
- **Back-compat:** legacy `AgentProfile` / skills without an `events.jsonl` (or empty stats) sync cleanly (serde defaults; empty log reduces to default stats).
- **Entitlement gate:** non-Pro account is refused before any network write.

---

## 10. Sequencing & honesty caveats

- **Depends on the Pattern → Skill migration.** This spec targets the unified skill
  corpus (`~/.mur/skills/<name>/` with `stats.yaml` + `events.jsonl`) from
  `2026-05-28-mur-notes-design.md` + Workflow Engine v2. As of 2026-05-29
  `SkillStats` and `notes_cmd` exist, but the `Pattern` type and `~/.mur/patterns/`
  are still live. Fleet-sync of the skill corpus should therefore be **sequenced
  after** v2 Pattern-removal + the Notes foundation land; it deliberately does not
  sync the legacy pattern tree. Until then, only agent profiles + already-skill
  entities are syncable.
- Independent of the TBD server-side draft *accept* endpoint (umbrella §10); fleet-sync uses its own fleet endpoints under `/api/v1/core/fleet/`.
- Commander relay / SSH-tunnel transport remains out of scope.
- Reconciling the stale `mur-commander` version note (umbrella §10) is **not** part of this work.
- v1 ships per-entity-type version counters; a unified fleet version vector is a candidate v2 refinement.

---

## 11. Open questions (for the plan)

- Exact wire shape of `FleetChange` payloads (full blob vs delta) — lean toward full-blob v1 for simplicity given small entity sizes.
- Whether installed-skill *content* (vs identity + version) needs to travel, or whether each device re-fetches skill content from its source — decide during planning.
- Manifest storage location/format under `~/.mur/.fleet-sync/` and migration from any existing `.sync_version`.
