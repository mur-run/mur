# Fleet-Sync (Pro) — Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship skill-corpus + agent-profile fleet-sync with conflict resolution (event-union + LWW).

**Spec:** `docs/superpowers/specs/2026-05-29-fleet-sync-pro-design.md`

**Dependency:** Pattern→Skill migration (Tier A ✅ complete)

**Tech Stack:** Rust 2024, existing server-sync + skill storage infrastructure.

---

## Architecture Overview

```
Phase 1 (✅): Agent profile + model binding sync via opaque blobs
  └─ `/api/v1/core/fleet/{entity_type}` endpoints
  └─ Base version conflict handling

Phase 2 (🔲): Skill corpus sync with evolved state merging
  ├─ Event-union merge: `events.jsonl` dedup + re-reduce
  ├─ Signed-manifest LWW: `skill.yaml` version-vector resolution
  ├─ Agent profile skill extension (via skill corpus)
  └─ Entitlement gate + degraded-mode for missing secrets
```

---

## Task Breakdown

### Phase 2A: Shared Types & Merge Logic (mur-common)

**Goal:** Event-union merge and signed-manifest LWW resolution logic, unit-testable independently.

#### Task 2A1: Event dedup + union types

**Files:**
- Modify: `mur-common/src/skill/mod.rs` — add event dedup types

**Steps:**
- [ ] Define `EventKey { ts, kind, outcome, source_device }` (stable dedup key)
- [ ] Define `EventUnionResult { merged_events: Vec<Event>, stats_delta: SkillStats }`
- [ ] Implement `union_events(local: Vec<Event>, remote: Vec<Event>) -> EventUnionResult`
  - Dedup by `EventKey`
  - Return sorted union
  - Compute stats delta (deterministic)
- [ ] Unit test: union commutativity (order doesn't matter)
- [ ] Unit test: idempotency (union-once = union-twice)
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2A1 — event union merge logic`

#### Task 2A2: Signed-manifest LWW resolution

**Files:**
- Modify: `mur-common/src/skill/mod.rs` — add manifest resolution

**Steps:**
- [ ] Define `ManifestResolution { winner: Manifest, reason: LwwReason }`
- [ ] Implement `resolve_manifest_lww(local: Manifest, remote: Manifest, force_local: bool) -> ManifestResolution`
  - Compare `updated_at` (version-vector)
  - Apply LWW (remote wins on tie-break)
  - Respect `--force-local` override
  - Validate signature is intact
- [ ] Unit test: LWW ordering (newer wins)
- [ ] Unit test: `--force-local` override
- [ ] Unit test: signature validation preserves integrity
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2A2 — manifest LWW resolution`

#### Task 2A3: Fleet sync types (opaque blobs)

**Files:**
- Modify: `mur-common/src/sync_types.rs` — add fleet types

**Steps:**
- [ ] Add `FleetEntityType::SkillCorpus` variant (joins existing `AgentProfile`, `ModelBinding`)
- [ ] Add `SkillCorpusDelta { skill_name, events_union, manifest_resolution }`
- [ ] Ensure serialization round-trips (events.jsonl + skill.yaml)
- [ ] Unit test: round-trip
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2A3 — fleet sync types`

---

### Phase 2B: Server-Side Endpoints (mur-server)

**Goal:** Skill corpus read/write under `/api/v1/core/fleet/skill-corpus`.

**Note:** Treats all blobs as opaque; merge logic stays in Rust client.

#### Task 2B1: Skill corpus store + handlers

**Files:**
- Modify: `internal/store/postgres/fleet_store.go` — add skill corpus handlers
- Modify: `internal/api/handlers/fleet.go` — add skill endpoints

**Steps:**
- [ ] Add `SaveSkillCorpus(user, skill_name, events_jsonl, manifest_yaml, base_version)` → (version, conflict)
- [ ] Add `GetSkillCorpus(user, skill_name, since_version)` → events + manifest + version
- [ ] Add handler `POST /api/v1/core/fleet/skill-corpus/{name}` (push)
- [ ] Add handler `GET /api/v1/core/fleet/skill-corpus?since=<version>` (pull list)
- [ ] Entitlement gate: check Pro tier before write
- [ ] Conflict detection: base_version check
- [ ] **Green checkpoint.** Commit: `feat(fleet-server): Phase 2B1 — skill corpus endpoints`

---

### Phase 2C: Client Sync Flow (mur-core)

**Goal:** Build skill corpus change list, sync with conflict retry.

#### Task 2C1: Skill corpus manifest + change builder

**Files:**
- Create: `mur-core/src/cmd/fleet_skill_sync.rs` (sibling to `fleet_sync.rs`)
- Modify: `mur-core/src/cmd/fleet_sync.rs` — add skill corpus dispatch

**Steps:**
- [ ] Define `SkillCorpusManifest { skills: HashMap<name, { sha256, events_tail, version }> }`
- [ ] Implement `load_skill_corpus_manifest() -> SkillCorpusManifest`
  - Scan `~/.mur/skills/`
  - Hash each `skill.yaml`
  - Read tail of `events.jsonl` (last-seen offset)
  - Load per-entity version from `~/.mur/.fleet-sync/skill-corpus-version`
- [ ] Implement `build_skill_corpus_changes(local: Manifest, previous: Manifest) -> Vec<SkillChange>`
  - Detect added/modified/deleted skills by hash
  - Package `skill.yaml` + `events.jsonl` delta as opaque blob
- [ ] Unit test: change detection on new skill
- [ ] Unit test: no-change when manifest stable
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2C1 — skill corpus manifest`

#### Task 2C2: Push flow with conflict retry

**Files:**
- Modify: `mur-core/src/cmd/fleet_skill_sync.rs` — add push

**Steps:**
- [ ] Implement `push_skill_corpus(changes, base_version, force_local) -> Result<new_version>`
  - POST to `/api/v1/core/fleet/skill-corpus` with base_version
  - On `{ version }`: update manifest, save new version
  - On `{ conflict }`: pull remote, merge (§6 flow below), retry
  - Retry loop: max 3 attempts, then surface clear error + `--force-local` hint
- [ ] Unit test: successful push updates manifest
- [ ] Unit test: conflict triggers retry
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2C2 — skill corpus push + retry`

#### Task 2C3: Pull flow with merge

**Files:**
- Modify: `mur-core/src/cmd/fleet_skill_sync.rs` — add pull

**Steps:**
- [ ] Implement `pull_skill_corpus(base_version, force_local) -> Result<()>`
  - GET from `/api/v1/core/fleet/skill-corpus?since=<base_version>`
  - For each skill in response:
    - If local skill exists: apply §6 merge flow (event-union + manifest LWW)
    - If local skill missing: write remote skill as-is
    - Validate signature integrity post-merge
    - Write merged `skill.yaml`, `events.jsonl`
    - Re-run reducer to regenerate `stats.yaml`
  - Update `~/.mur/.fleet-sync/skill-corpus-version`
- [ ] Unit test: pull new skill (fresh on device B)
- [ ] Unit test: merge skill after divergence
- [ ] Unit test: stats regenerated correctly post-merge
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2C3 — skill corpus pull + merge`

#### Task 2C4: Entitlement gate + degraded mode

**Files:**
- Modify: `mur-core/src/cmd/fleet_sync.rs` — add entitlement check
- Modify: `mur-core/src/cmd/sync_cmd.rs` — add status reporting

**Steps:**
- [ ] Implement `check_pro_entitlement() -> Result<()>`
  - GET `/api/v1/core/auth/me` → check `tier == Pro`
  - Short-circuit with upgrade message if not Pro
- [ ] Implement `report_degraded_skills() -> Vec<DegradedAgent>`
  - Scan skills for missing secret-refs
  - Report via `mur sync status`
- [ ] Sync succeeds even if secret-refs unresolved (degraded agent loads unbound)
- [ ] Unit test: non-Pro rejected at gate
- [ ] Unit test: degraded skill surfaces in status
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2C4 — entitlement gate + degraded mode`

#### Task 2C5: CLI surface + dispatch

**Files:**
- Modify: `mur-core/src/cli/mod.rs` — extend `SyncAction`
- Modify: `mur-core/src/cmd/sync_cmd.rs` — dispatch to fleet_skill_sync

**Steps:**
- [ ] Extend `SyncAction::Fleet` to include `SkillCorpus` variant
- [ ] `mur sync fleet [--pull | --push | --both] [--force-local]`
- [ ] `mur sync status` — show per-entity drift (agent profile vs server version, skill-corpus events-tail vs server tail)
- [ ] Help text: explain flux behavior (events always merge, manifests resolve by LWW, `--force-local` as override)
- [ ] **Green checkpoint.** Commit: `feat(fleet): Phase 2C5 — CLI surface for skill corpus sync`

---

### Phase 2D: Integration Tests

**Goal:** Two-device round-trip with divergence, merge, and conflict resolution.

#### Task 2D1: Two-device event merge

**Files:**
- Create: `mur-core/tests/fleet_skill_merge_test.rs`

**Steps:**
- [ ] Setup: create test skill on device A
- [ ] Device A: record usage event
- [ ] Device B: pull skill → events merge → stats regenerated
- [ ] Assert: both devices' usage histories combined
- [ ] Assert: `stats.yaml` usage_count reflects union
- [ ] **Green checkpoint.** Commit: `test(fleet): Phase 2D1 — two-device event merge`

#### Task 2D2: Two-device manifest LWW

**Files:**
- Modify: `mur-core/tests/fleet_skill_merge_test.rs`

**Steps:**
- [ ] Device A: push initial skill.yaml v1
- [ ] Device B: pull, get v1
- [ ] Device A: edit skill, push v2
- [ ] Device B: edit different field, try push (conflict)
  - Conflict pull returns v2 from device A
  - LWW resolves to v2 (newer `updated_at`)
  - Device B's local edit is lost (expected — manifest is signed unit, can't be field-merged)
  - Device B surfaces message: "Your local edit was superseded; manifest is v2 from device A"
- [ ] Device B retries with empty change list (no conflict)
- [ ] **Green checkpoint.** Commit: `test(fleet): Phase 2D2 — manifest LWW conflict resolution`

#### Task 2D3: Secret-ref degraded mode

**Files:**
- Modify: `mur-core/tests/fleet_skill_merge_test.rs`

**Steps:**
- [ ] Device A: create agent with model binding (secret-ref to GPT-4 in local keychain)
- [ ] Device B: pull agent profile → secret-ref present, but no matching keychain entry
- [ ] Assert: sync succeeds (no write error)
- [ ] Assert: `mur sync status` reports agent as degraded
- [ ] Assert: agent loads, but model binding is unbound (graceful degradation)
- [ ] Device B: user adds secret to keychain
- [ ] Device B: model binding resolves, agent ready
- [ ] **Green checkpoint.** Commit: `test(fleet): Phase 2D3 — secret-ref degraded mode`

#### Task 2D4: Entitlement gate

**Files:**
- Modify: `mur-core/tests/fleet_skill_merge_test.rs`

**Steps:**
- [ ] Mock non-Pro entitlement response
- [ ] Call `mur sync fleet --push`
- [ ] Assert: rejected with clear upgrade message (not a cryptic error)
- [ ] **Green checkpoint.** Commit: `test(fleet): Phase 2D4 — entitlement gate`

---

### Phase 2E: Documentation & Migration Notes

#### Task 2E1: Update README + docs

**Files:**
- Modify: `README.md` — add fleet-sync section
- Create: `docs/superpowers/guides/fleet-sync-setup.md`

**Steps:**
- [ ] Fleet-sync intro (Pro feature, cross-device skill sync)
- [ ] Setup steps (sign in, enable on device B, choose pull/push/both)
- [ ] Troubleshooting: degraded agents, conflict hints
- [ ] **Green checkpoint.** Commit: `docs: fleet-sync Phase 2 guide`

#### Task 2E2: Migration notes (Pattern removal)

**Files:**
- Modify: `docs/superpowers/MIGRATION-STATUS.md`

**Steps:**
- [ ] Update Tier B status: Phase 2 complete
- [ ] Note: Tier C can now proceed (Pattern type removal)
- [ ] **Green checkpoint.** Commit: `docs: mark Tier B complete`

---

## Testing Checklist

- [ ] Unit tests for event-union dedup + commutativity
- [ ] Unit tests for manifest LWW ordering
- [ ] Unit tests for signature integrity post-merge
- [ ] Integration: two-device event merge
- [ ] Integration: manifest LWW conflict resolution
- [ ] Integration: secret-ref degraded mode
- [ ] Integration: entitlement gate rejects non-Pro
- [ ] Integration: `mur sync status` reports drift
- [ ] Cargo test: all pass
- [ ] Cargo clippy: clean
- [ ] Cargo fmt: clean

---

## Rollout & Monitoring

1. **Feature flag** (optional): gate skill-corpus sync behind a feature flag if server-side rollout is staggered
2. **Pro-only:** entitlement check happens client-side and server-side
3. **Monitoring:** track sync success rate, conflict rates, mean time to resolution
4. **Degraded-mode visibility:** `mur sync status` must be prominent (users need to know agents are unbound waiting for secrets)

---

## Success Criteria

✅ **Skill corpus syncs** across two devices with event-union merge  
✅ **Manifest conflicts** resolved by LWW (with `--force-local` override)  
✅ **Agent profiles** sync with secret-refs (not plaintext)  
✅ **Pro entitlement** enforced (non-Pro rejected before network call)  
✅ **Degraded mode** surfaces clearly when secrets missing  
✅ **Two-device round-trip** test passes (diverge → merge → converge)  
✅ **All tests pass** (unit + integration)  
✅ **CLI surface** documented and intuitive  

---

## References

- **Spec:** `docs/superpowers/specs/2026-05-29-fleet-sync-pro-design.md`
- **Phase 1 (shipped):** PR #309, `docs/superpowers/plans/2026-05-29-fleet-sync-pro-phase1.md`
- **Pattern migration (unblocks this):** Tier A ✅ complete
