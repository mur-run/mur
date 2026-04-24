# murmur `mur agent rekey` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-04-24-murmur-agent-rekey-design.md`
**Base branch:** this repo `main` (after PR #29 merges); `mur-commander` `main` (after PR #11 merges)
**New branches:**
- `feat/agent-rekey` off `main` in mur
- `feat/agent-rekey-commander` off `main` in mur-commander

**Done definition:** 6 milestones, each is a standalone PR. Every milestone leaves the workspace green (all tests + clippy + fmt). The M5 split-detection test demonstrates the full security property end-to-end.

---

## Milestone map

| M | Title | Deps | PR size |
|---|---|---|---|
| M1 | Schema + attestation primitives + `mur agent rekey` | — | medium |
| M2 | Commander `apply_rotation` + grace-period registry | M1 | medium |
| M3 | TcpConnector fallback + Agent Card `previous_pubkey` | M2 | small |
| M4 | `--emergency` + `murc agent approve-rekey` | M2 | medium |
| M5 | Split detection + attestation chain verification | M2 | small |
| M6 | Grace-expiry cleanup + `rekey-status` CLI + docs | M1-M5 | small |

---

## Milestone M1 — Schema + attestation primitives + `mur agent rekey`

### Task M1.1 — Extend `IdentityConfig` schema

**Files:**
- Modify: `mur-common/src/agent.rs`
- Test: `mur-common/tests/profile_schema.rs` (extend)
- Create: `mur-common/tests/fixtures/profile_p0a6_rotated.yaml`

- [ ] Add fields per spec: `algorithm`, `key_version`, `created_at_key`, `previous_pubkey`, `previous_key_version`, `grace_expires_at`, `rotated_at`. All `#[serde(default)]`.
- [ ] Add `SUPPORTED_ALGORITHMS: &[&str] = &["ed25519"]` constant.
- [ ] Add 3 fixture-based tests: bare P0a.5 profile (no new fields) loads with defaults; P0a.6-rotated profile round-trips; unknown algorithm is accepted at parse time (runtime guard is separate).
- [ ] Commit: `feat(common): IdentityConfig rekey extensions (algorithm, key_version, previous_pubkey, grace)`

### Task M1.2 — `RotationAttestation` type + sign/verify

**Files:**
- Modify: `mur-common/src/identity.rs`
- Create: `mur-common/tests/rotation_attestation.rs`

- [ ] Define `RotationAttestation` struct per spec.
- [ ] Define `RotationReason` enum (Scheduled / SuspectCompromise / OwnerChange / Emergency) with `rename_all = "snake_case"`.
- [ ] Implement `RotationAttestation::canonical_bytes()` — serde_json with sorted keys, no whitespace, signature field cleared. This is what gets signed.
- [ ] Implement `RotationAttestation::sign(&mut self, signing_key: &SigningKey) -> Result<(), IdentityError>` that sets `self.signature` to multibase base58btc of the Ed25519 signature.
- [ ] Implement `RotationAttestation::verify(&self, old_pubkey: &str) -> Result<(), IdentityError>` — decode multibase, verify against `old_pubkey`.
- [ ] Tests:
  - sign then verify round-trip
  - tampered `new_pubkey` fails verify
  - wrong `old_pubkey` fails verify
  - empty signature fails verify unless `reason = Emergency`
  - `Emergency` reason allows empty signature and `verify_emergency()` accepts it
- [ ] Commit: `feat(common): RotationAttestation with Ed25519 sign/verify (canonical JSON)`

### Task M1.3 — Bootstrap attestation on `mur agent create`

**Files:**
- Modify: `mur-core/src/cmd/agent.rs`
- Test: `mur-core/tests/agent_create_identity.rs` (extend)

- [ ] After writing identity.pub in `cmd_create`, also write `rotations.jsonl` with a bootstrap line: `{"schema":1,"uuid":..., "algorithm":"ed25519", "old_pubkey":"", "new_pubkey":<pubkey>, "old_key_version":0, "new_key_version":0, "rotated_at":<now>, "reason":"scheduled", "signature":"", "bootstrap":true}` (note: we need a `bootstrap: bool` field on the attestation struct too; add it with `#[serde(default)]`).
- [ ] Populate `IdentityConfig.algorithm = "ed25519"`, `key_version = 0`, `created_at_key = now`.
- [ ] Extend test to verify `rotations.jsonl` exists and contains exactly one line.
- [ ] Commit: `feat(core): bootstrap rotation attestation on mur agent create`

### Task M1.4 — `mur agent rekey <name>` CLI (normal path only)

**Files:**
- Create: `mur-core/src/cmd/agent_rekey.rs`
- Modify: `mur-core/src/main.rs` / clap wiring
- Create: `mur-core/tests/agent_rekey_cli.rs`

- [ ] New subcommand `agent rekey` with args: `<name>`, `--reason scheduled|suspect-compromise|owner-change` (default `scheduled`), `--yes` (skip interactive confirm), `--emergency` (present but errors in M1 — "--emergency path ships in M4").
- [ ] Logic: read current `profile.yaml`, read `identity.key` + `identity.pub`, generate new keypair, build attestation, sign with old key, atomic file rotation, update `profile.yaml`, append to `rotations.jsonl`, write `identity.attestation.json`.
- [ ] Atomic rotation order (crash-safe):
  1. write `identity.key.new` (0600)
  2. write `identity.pub.new`
  3. write `identity.attestation.new.json`
  4. append to `rotations.jsonl.tmp`
  5. `rename identity.key -> identity.key.prev`
  6. `rename identity.pub -> identity.pub.prev`
  7. `rename identity.key.new -> identity.key`
  8. `rename identity.pub.new -> identity.pub`
  9. `rename identity.attestation.new.json -> identity.attestation.json`
  10. append to `rotations.jsonl`
  11. rewrite `profile.yaml` atomically (temp + rename)
- [ ] SIGTERM current runtime process (read pid from `running.lock`) and wait up to 5s for it to exit. If still running, warn but proceed — supervisor symlink will restart it.
- [ ] Test: create agent → rekey → verify: new key on disk, prev key on disk, `rotations.jsonl` has 2 lines, profile has updated `previous_pubkey` + `key_version=1` + `grace_expires_at` set 30 days out.
- [ ] Test: run rekey twice in a row → 3 lines in jsonl, `key_version=2`, `previous_pubkey` reflects v1's key (not v0).
- [ ] Test: `--reason suspect-compromise` is reflected in attestation + jsonl line.
- [ ] Commit: `feat(core): mur agent rekey — normal rotation with attestation`

### Task M1.5 — Verification checkpoint

- [ ] `cargo test --workspace` green in mur.
- [ ] `cargo clippy --workspace -- -D warnings` clean.
- [ ] `cargo fmt --check` clean.
- [ ] Manual smoke: create + rekey + verify jsonl chain.
- [ ] Push branch, open PR.

---

## Milestone M2 — Commander `apply_rotation` + grace registry

### Task M2.1 — Extend `RegisteredAgent`

**Files:**
- Modify: `mur-commander/crates/engine/src/a2a/discovery.rs`
- Test: `mur-commander/crates/engine/tests/agent_registry_uuid.rs` (extend)

- [ ] Add fields: `algorithm`, `key_version`, `previous_pubkey`, `previous_key_version`, `grace_expires_at`, `rotation_count`, `last_rotation_at`, `compromised_marker`. All `#[serde(default)]`.
- [ ] Add `CompromisedMarker` struct.
- [ ] Extend existing "legacy loads" test to confirm new fields default cleanly.
- [ ] Commit: `feat(engine): RegisteredAgent rotation bookkeeping fields`

### Task M2.2 — `AgentRegistry::apply_rotation`

**Files:**
- Modify: `mur-commander/crates/engine/src/a2a/discovery.rs`
- Test: `mur-commander/crates/engine/tests/apply_rotation.rs` (new)

- [ ] Signature: `pub fn apply_rotation(&self, uuid: &str, att: &RotationAttestation) -> Result<RotationOutcome, RotationError>`
- [ ] `RotationOutcome`: `Applied` | `AlreadyApplied` (idempotent — same attestation applied twice is a no-op) | `Quarantined(CompromisedMarker)`
- [ ] `RotationError`: `UnknownAgent` | `OldKeyMismatch` | `VersionMismatch` | `BadSignature` | `EmergencyRequiresApproval` | `Io(...)`
- [ ] Logic:
  1. Find entry by UUID; 404 → `UnknownAgent`.
  2. Check `att.old_pubkey == entry.pubkey`; else if it matches previous within grace → accept as idempotent replay; else `OldKeyMismatch` → record `CompromisedMarker` with reason `OldKeyMismatch`.
  3. Check `att.old_key_version == entry.key_version`; else `VersionMismatch` → split-detection path (covered in M5).
  4. If `att.reason == Emergency` → return `EmergencyRequiresApproval` without mutating.
  5. Call `att.verify(&att.old_pubkey)`; on failure → `BadSignature` + audit event.
  6. On success: update entry (`previous_pubkey`, `previous_key_version`, `pubkey`, `key_version`, `grace_expires_at = now + 30d`, `last_rotation_at = now`, `rotation_count += 1`), save.
- [ ] Tests: successful rotation; bad signature; version mismatch; idempotent replay; emergency rejected without approval; unknown agent.
- [ ] Commit: `feat(engine): AgentRegistry::apply_rotation with grace-period TTL`

### Task M2.3 — `murmur_bridge` reads attestation + calls `apply_rotation`

**Files:**
- Modify: `mur-commander/crates/engine/src/remote/murmur_bridge.rs`
- Test: `mur-commander/crates/engine/tests/murmur_bridge.rs` (extend)

- [ ] On `running.lock` MODIFY event, also read `identity.attestation.json` from the same agent dir.
- [ ] If attestation present and `new_key_version > entry.key_version`, call `apply_rotation`.
- [ ] If absent or older, continue with existing `upsert` path (covers first-time registration and non-rotation updates).
- [ ] New test: start bridge, simulate rekey by writing new attestation + updated running.lock, assert registry reflects key_version=1 + previous_pubkey set.
- [ ] Commit: `feat(engine): murmur_bridge applies rotation attestations from agent FS`

### Task M2.4 — Verification checkpoint

- [ ] `cargo test -p mur-engine` green.
- [ ] Push commander branch, open PR draft (depends on M1 merge first for `mur-common` alignment).

---

## Milestone M3 — TcpConnector fallback + Agent Card previous_pubkey

### Task M3.1 — Agent Card publishes `previous_pubkey`

**Files:**
- Modify: `mur-agent-runtime/src/protocol/methods/card.rs`
- Test: `mur-agent-runtime/tests/card_extended.rs` (extend)

- [ ] When `profile.inner.identity.previous_pubkey.is_some()` and `grace_expires_at > now`, include `previous_pubkey` (string) and `grace_expires_at` (string) in the card JSON.
- [ ] Test: rotated profile → card contains both; post-grace profile → card omits them.
- [ ] Commit: `feat(agent-runtime): Agent Card publishes previous_pubkey during grace`

### Task M3.2 — TcpConnector fallback dial

**Files:**
- Modify: `mur-agent-runtime/src/transport/tcp.rs`
- Test: `mur-agent-runtime/tests/tcp_transport.rs` (extend)

- [ ] New method `TcpConnector::dial_with_fallback(addr, identity, candidates: &[[u8; 32]]) -> io::Result<Self>`.
- [ ] Iterates candidates in order; first successful handshake wins.
- [ ] Keep existing `dial` API as a wrapper that passes a single-element slice.
- [ ] Test: 2 agents share identity B's old pubkey; after B rekeys to new pubkey, dial_with_fallback([new, old]) succeeds on the first element if both accepted; succeeds on fallback if server uses old accepting.
- [ ] Commit: `feat(agent-runtime): TcpConnector::dial_with_fallback for rekey migration`

### Task M3.3 — Verification checkpoint

- [ ] Full workspace green.
- [ ] Commit, push, PR.

---

## Milestone M4 — `--emergency` + `murc agent approve-rekey`

### Task M4.1 — `mur agent rekey --emergency` path

**Files:**
- Modify: `mur-core/src/cmd/agent_rekey.rs`

- [ ] Interactive TTY prompt requiring typed `I UNDERSTAND`.
- [ ] Build attestation with `reason = Emergency`, `signature = ""`, `bootstrap = false`.
- [ ] Set `profile.identity.emergency_rekey_at = now` (new optional field — add to schema; back-compat).
- [ ] Continue same file-rotation sequence.
- [ ] Test: emergency rekey succeeds on disk; attestation has no signature; jsonl line flagged.
- [ ] Commit: `feat(core): mur agent rekey --emergency path`

### Task M4.2 — `murc agent approve-rekey <uuid>` CLI

**Files:**
- Modify: `mur-commander/crates/cli/src/` (add subcommand group if missing)
- New: `mur-commander/crates/engine/src/a2a/discovery.rs` — `approve_emergency_rotation(uuid)` method
- Test: `mur-commander/crates/engine/tests/apply_rotation.rs` (extend)

- [ ] `approve_emergency_rotation` requires the same attestation path on disk that triggered `PendingEmergencyApproval` — re-reads, validates `reason = Emergency`, then applies bypassing signature check. Writes audit event.
- [ ] CLI verifies caller has write access to `~/.mur/commander/agents.json` (option (a): FS gate).
- [ ] Test: pending-approval entry + approve CLI → registry updated; pending-approval entry + reject CLI → entry deleted or marked rejected.
- [ ] Commit: `feat(commander): murc agent approve-rekey for emergency rotations`

### Task M4.3 — `murmur_bridge` treats emergency attestation as pending

**Files:**
- Modify: `mur-commander/crates/engine/src/remote/murmur_bridge.rs`

- [ ] On emergency attestation, set `RegisteredAgent.compromised_marker = Some({ reason: "pending_emergency_approval", ... })` and emit high-priority audit event.
- [ ] Do not update `pubkey`.
- [ ] Test.
- [ ] Commit: `feat(engine): murmur_bridge holds emergency rotations pending admin approval`

### Task M4.4 — Verification checkpoint

- [ ] Full cross-repo test: emergency rotation is blocked until `approve-rekey` runs; peer handshakes fail closed.

---

## Milestone M5 — Split detection + chain verification

### Task M5.1 — Attestation chain verifier

**Files:**
- New: `mur-common/src/identity_chain.rs`
- Test: `mur-common/tests/identity_chain.rs`

- [ ] `verify_chain(bootstrap_pubkey: &str, chain: &[RotationAttestation]) -> Result<ChainOutcome, IdentityError>` walks the chain, verifying each attestation against the preceding `new_pubkey`.
- [ ] Handles bootstrap (key_version=0, no signature).
- [ ] Tests: valid 5-step chain; inject tamper; missing version; duplicate version; reordered → all caught.
- [ ] Commit: `feat(common): RotationAttestation chain verifier`

### Task M5.2 — Commander runs chain check on bridge

**Files:**
- Modify: `mur-commander/crates/engine/src/a2a/discovery.rs`
- Modify: `mur-commander/crates/engine/src/remote/murmur_bridge.rs`
- Test: `mur-commander/crates/engine/tests/apply_rotation.rs` (extend)

- [ ] On registry first-seeing an agent, read the full `rotations.jsonl` from agent dir and run `verify_chain`; store the verified `key_version → pubkey` mapping in registry.
- [ ] When `apply_rotation` gets a `VersionMismatch` (attestation's `old_key_version < registry.key_version`, but `new_key_version == registry.key_version`), trigger split detection: mark `compromised_marker` with `reason = "split_attestation_vN"` + list conflicting `new_pubkey`s.
- [ ] Quarantined agents' cards/pubkeys are unchanged; but `compromised_marker` is included in card so peers know to refuse trust.
- [ ] Tests: two rotations replayed off same version → quarantine; single rotation with correct chain → clean.
- [ ] Commit: `feat(engine): split-attestation detection + quarantine marker`

### Task M5.3 — Verification checkpoint

- [ ] 100-step chain test passes.
- [ ] Split scenario test passes.

---

## Milestone M6 — Grace cleanup + `rekey-status` CLI + docs

### Task M6.1 — Agent-side grace expiry cleanup

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`
- Test: `mur-agent-runtime/tests/grace_cleanup.rs` (new)

- [ ] On supervisor startup, if `identity.grace_expires_at < now`:
  - Shred `identity.key.prev` (Unix: `shred -u`; Windows: overwrite then delete).
  - Remove `identity.pub.prev`.
  - Clear `IdentityConfig.previous_pubkey`, `previous_key_version`, `grace_expires_at` and save profile atomically.
- [ ] Also run hourly while running.
- [ ] Test with fake `grace_expires_at` in the past: verify files gone + profile updated.
- [ ] Commit: `feat(agent-runtime): grace-period expiry shreds previous identity`

### Task M6.2 — Commander-side grace registry cleanup

**Files:**
- Modify: `mur-commander/crates/engine/src/a2a/discovery.rs`
- Modify: `mur-commander/crates/daemon/src/main.rs` (schedule hourly task)

- [ ] `AgentRegistry::sweep_grace_expiries()` clears `previous_pubkey` + `previous_key_version` + `grace_expires_at` for any entry where `grace_expires_at < now`.
- [ ] Daemon schedules hourly call.
- [ ] Test.
- [ ] Commit: `feat(commander): hourly grace-period sweep clears expired previous_pubkey`

### Task M6.3 — `mur agent rekey-status <name>` CLI

**Files:**
- Modify: `mur-core/src/cmd/agent_rekey.rs`

- [ ] Reads profile + last attestation + jsonl tail; prints:
  - Current `key_version`, `pubkey`, `algorithm`, `created_at_key`
  - Previous `pubkey` + `grace_expires_at` (or "no previous key in grace")
  - Rotation history count (from jsonl line count)
  - Any `emergency_rekey_at` marker
- [ ] JSON mode via `--json`.
- [ ] Test.
- [ ] Commit: `feat(core): mur agent rekey-status`

### Task M6.4 — Docs + COMPLETE log

**Files:**
- Modify: `CLAUDE.md` (add rekey section under P0a.5 extensions)
- Create: `docs/superpowers/plans/2026-04-24-murmur-agent-rekey-plan-COMPLETE.md`

- [ ] Update CLAUDE.md with CLI commands + file layout + grace semantics.
- [ ] Write COMPLETE log mirroring style of the P0a.5 COMPLETE doc.
- [ ] Commit: `docs: agent rekey plan COMPLETE + CLAUDE.md updates`

### Task M6.5 — Final verification + PR

- [ ] Run cross-repo E2E: create → rekey → rekey → rekey → verify chain; emergency rekey + approve; grace expiry cleanup.
- [ ] `cargo test --workspace` in both repos.
- [ ] Open final PR batch, link to spec.

---

## Risk register

| Risk | Mitigation |
|---|---|
| Crash mid-rotation (between file renames) leaves partial state | Atomic rename ordering (M1.4); startup reconciler reads `rotations.jsonl` as source of truth and rebuilds on mismatch |
| Emergency approval CLI runs on wrong host | Check requires FS write access to `~/.mur/commander/agents.json`; option (a) per spec |
| Clock skew between agent + commander breaks `grace_expires_at` comparisons | Use server's own clock when making decisions; tolerance ≥ 1 hour |
| Large `rotations.jsonl` on very old agents | ~300 bytes per line; 1000 rotations = 300 KB; not a concern |
| Attestation JSON canonical serialization mismatch between implementations | Use a single helper in `mur-common`; both signer and verifier call it |
| 30-day grace too long for some ops | Configurable via `[identity] grace_period_days` (1..=90) |

---

## Exit criteria

- [ ] All M1-M6 merged to main in both repos.
- [ ] `mur agent rekey` + `mur agent rekey --emergency` + `mur agent rekey-status` CLI shipped and documented.
- [ ] `murc agent approve-rekey` + `reject-rekey` shipped.
- [ ] Commander registry tracks `key_version` + grace + quarantine correctly.
- [ ] Split detection test passes.
- [ ] Grace cleanup verified on Unix (and guarded on Windows).
- [ ] CLAUDE.md references both spec and plan.
