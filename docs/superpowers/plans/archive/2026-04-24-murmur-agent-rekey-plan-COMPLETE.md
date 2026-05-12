# murmur `mur agent rekey` — Implementation Complete

**Status:** All 24 tasks shipped across 6 milestones in two repos.
**Date:** 2026-04-25
**Branches:**
- `feat/agent-rekey` in `mur-run/mur` (off `feat/murmur-p0a`) — PR [#30](https://github.com/mur-run/mur/pull/30)
- `feat/agent-rekey-commander` in `mur-run/mur-commander` (off `main`) — PR [#12](https://github.com/mur-run/mur-commander/pull/12)

## Scope

Add per-agent Ed25519 identity rotation as P0a.6, building on the P0a.5 identity foundation. Resolves Open Question Q-B from the fleet architecture spec.

Implements:
- **Normal rotation** — signed attestation chain rooted at agent create time; commander auto-applies; 30-day grace window.
- **Emergency rotation** — when the old key is unrecoverable; written unsigned, must be explicitly approved on the commander host (option-a FS-gated).
- **Split detection** — diverging attestations on the same `key_version` boundary quarantine the agent.
- **Peer migration** — `TcpConnector::dial_with_fallback` accepts multiple candidate pubkeys; Agent Card publishes both during grace.
- **Grace cleanup** — agent shreds `identity.key.prev` on supervisor startup; commander sweeps expired `previous_pubkey` hourly.

## Phase roll-up

### M1 — Schema + attestation primitives + `mur agent rekey` (mur)

5 tasks. Key artefacts:

- `mur-common::agent::IdentityConfig` — extended with `algorithm`, `key_version`, `created_at_key`, `previous_pubkey`, `previous_key_version`, `grace_expires_at`, `rotated_at`, `emergency_rekey_at`. Manual `Default` impl preserves `algorithm = "ed25519"`.
- `mur-common::identity::RotationAttestation` — schema-1 attestation with sorted-key canonical JSON (`canonical_bytes`); `RotationReason` enum (Scheduled / SuspectCompromise / OwnerChange / Emergency); `sign()` + `verify()` + `verify_or_emergency()`; bootstrap entries skip verification.
- `mur agent create` (M1.3) — writes a bootstrap rotation line into `<agent_dir>/rotations.jsonl` and populates the new IdentityConfig fields.
- `mur agent rekey <name>` (M1.4) — CLI for normal rotation: signs an attestation with the OLD key, atomically rotates `identity.{key,pub}` to `.prev`, writes new keypair, appends to `rotations.jsonl`, updates `profile.yaml`, SIGTERMs the running runtime.
- `--emergency` flag wired in M4.

**Tests added (M1):** 11 profile_schema + 10 rotation_attestation + 4 agent_rekey_cli = 25 new tests.

### M2 — Commander `apply_rotation` + grace registry (mur-commander)

3 tasks. Key artefacts:

- `engine::a2a::discovery::RegisteredAgent` — extended with `algorithm`, `key_version`, `previous_pubkey`, `previous_key_version`, `grace_expires_at`, `rotation_count`, `last_rotation_at`, `compromised_marker`. `CompromisedMarker` struct.
- `engine::a2a::discovery::AgentRegistry::apply_rotation` — idempotent + signature-verified rotation entry point. Returns `RotationOutcome::{Applied, AlreadyApplied, Quarantined}` or `RotationError::{UnknownAgent, OldKeyMismatch, VersionMismatch, BadSignature, EmergencyRequiresApproval, Io}`. 30-day default grace.
- `engine::remote::murmur_bridge` — when it sees `running.lock` change, also reads `identity.attestation.json` next to it and calls `apply_rotation` if `new_key_version > registry.key_version`.

**Tests added (M2):** 1 new agent_registry_uuid + 8 apply_rotation + 1 murmur_bridge integration = 10 new tests.

### M3 — Agent Card previous_pubkey + TcpConnector fallback (mur)

3 tasks. Key artefacts:

- `mur-agent-runtime::protocol::methods::card::CardHandler` — publishes `algorithm`, `key_version`, and (during grace) `previous_pubkey` + `previous_key_version` + `grace_expires_at`.
- `mur-agent-runtime::transport::tcp::TcpConnector::dial_with_fallback` — iterates candidate pubkeys in order; first successful Noise XK handshake wins. Backward-compatible `dial` wrapper.

**Tests added (M3):** 2 new card_extended + 3 new tcp_transport = 5 tests.

### M4 — Emergency path + `murc agent approve-rekey` (both repos)

4 tasks. Key artefacts:

- `mur agent rekey --emergency` — interactive `I UNDERSTAND` prompt; writes unsigned attestation; sets `profile.identity.emergency_rekey_at`.
- `engine::a2a::discovery::AgentRegistry::approve_emergency_rotation` — applies an unsigned emergency attestation (still validates `old_pubkey` + `old_key_version`); skips signature check; clears `compromised_marker`.
- `reject_emergency_rotation` — clears `pending_emergency_approval` marker without applying.
- `murmur_bridge` — on `EmergencyRequiresApproval`, sets `compromised_marker = pending_emergency_approval` and emits a high-priority audit log; agent's pubkey/key_version stay UNCHANGED.
- `murc agent approve-rekey <uuid>` / `reject-rekey <uuid>` CLI — gated by FS write access to `~/.mur/commander/agents.json` (option-a per spec). Locates the unsigned attestation by scanning `<MUR_HOME>/agents/*/identity.attestation.json`.

**Tests added (M4):** 2 new agent_rekey_cli (mur) + 4 new apply_rotation (commander) = 6 tests.

### M5 — Split detection + chain verification (both repos)

3 tasks. Key artefacts:

- `mur-common::identity::verify_chain(chain, opts)` — walks an attestation chain top-to-bottom (bootstrap → v1 → v2 → ...). Validates `+1` succession, pubkey continuity, no duplicate versions, signature on each non-bootstrap step. `ChainOptions { allow_emergency }` controls whether emergency entries are accepted in-chain.
- `engine::a2a::discovery::apply_rotation` (M5.2) — split-attestation detection: when an attestation arrives claiming the same `new_key_version` already in the registry but with a DIFFERENT `new_pubkey`, the agent is quarantined with `compromised_marker.reason = "split_attestation_vN_to_vN+1"` and the conflicting pubkeys are recorded.

**Tests added (M5):** 11 identity_chain + 1 split-attestation = 12 tests.

### M6 — Grace cleanup + `rekey-status` + docs (both repos)

5 tasks. Key artefacts:

- `mur-agent-runtime::supervisor::grace_cleanup_if_expired` — on every supervisor startup, if `grace_expires_at` has passed, shreds `identity.key.prev` (best-effort `shred -u`, falls back to overwrite + unlink) and clears `previous_*` fields from `profile.yaml`.
- `engine::a2a::discovery::AgentRegistry::sweep_grace_expiries` — clears `previous_pubkey` / `previous_key_version` / `grace_expires_at` on every entry whose grace window has passed. Returns count swept.
- `mur-commander/crates/daemon` — hourly task spawned at startup that calls `sweep_grace_expiries`.
- `mur agent rekey-status <name> [--json]` — text or JSON dump of current/previous keys, algorithm, grace remaining (in days), rotation history line count, optional emergency marker.
- This file + CLAUDE.md "P0a.6 additions" section.

**Tests added (M6):** 3 grace_cleanup (agent) + 2 sweep tests + 2 rekey-status CLI = 7 tests.

## Tests added across the rekey work

| Crate | File | Count |
|---|---|---|
| mur-common | tests/profile_schema.rs (extended) | +3 |
| mur-common | tests/rotation_attestation.rs | 10 |
| mur-common | tests/identity_chain.rs | 11 |
| mur-core | tests/agent_create_identity.rs (extended) | +1 |
| mur-core | tests/agent_rekey_cli.rs | 7 |
| mur-agent-runtime | tests/card_extended.rs (extended) | +2 |
| mur-agent-runtime | tests/tcp_transport.rs (extended) | +3 |
| mur-agent-runtime | tests/grace_cleanup.rs | 3 |
| engine (commander) | tests/agent_registry_uuid.rs (extended) | +1 |
| engine (commander) | tests/apply_rotation.rs | 15 |
| engine (commander) | tests/murmur_bridge.rs (extended) | +1 |

**Total: 57 new tests across the two repos**, all green. Pre-existing ~770 + ~777 lib tests still pass. Clippy + fmt clean for touched code.

## Deviations from the plan (all benign)

1. **mur-common chain verifier — `ChainOptions::default()` derived.** Plan specimen had a manual `impl Default`; clippy `derivable_impls` flagged it. Replaced with `#[derive(Default)]` since `bool::default()` is `false`.
2. **mur-commander edition 2021 — nested `if let` not let-chains.** Same workaround as P0a.5: the commander workspace is on edition 2021 and let-chains are not stabilized; the rekey code uses nested `if let` blocks.
3. **CLI `rekey_emergency_flag_errors_in_m1` test renamed.** The original M1.4 test asserted that `--emergency` errored; once M4.1 made it valid, the test was rewritten to assert the success path (`rekey_emergency_writes_unsigned_attestation`) and a new `rekey_emergency_aborts_without_confirmation_phrase` was added for the abort-without-`I UNDERSTAND` path.
4. **`mur-common` workspace dep on commander side bumped twice.** First pin `rev = "1b1b013"` after M1.4 (RotationAttestation types); bumped to `rev = "2b51b1e"` after M5.1 (chain verifier). Both pins TODO'd to bounce back to a v2.x.x tag once mur cuts a release.
5. **Single `Agent` subcommand group in commander CLI.** The plan reserved space for more agent-management commands later; for now `murc agent` only has `approve-rekey` and `reject-rekey`.

## Commits (chronological)

### mur (`feat/agent-rekey`)

```
a4e6ea2 feat(core): mur agent rekey-status (M6.3) — text + --json output
82d9b81 fix(agent-runtime): collapse nested if let in shred_file (clippy)
1bcc577 feat(agent-runtime): grace expiry shreds identity.key.prev + clears profile (M6.1)
2b51b1e feat(common): RotationAttestation chain verifier (M5.1)
f16e826 feat(core): mur agent rekey --emergency path (M4.1)
d6a91c2 feat(agent-runtime): TcpConnector::dial_with_fallback for rekey grace migration (M3.2)
2e0f552 feat(agent-runtime): Agent Card publishes previous_pubkey + key_version (M3.1)
1b1b013 feat(core): mur agent rekey — normal rotation with attestation
cfa1f8e feat(core): bootstrap rotation attestation on mur agent create
506778c style: cargo fmt (pre-existing P0a.5 drift)
b6baaef feat(common): RotationAttestation with Ed25519 sign/verify (canonical JSON)
96f965e feat(common): IdentityConfig rekey extensions (algorithm, key_version, previous_pubkey, grace)
9a8571c docs(spec): mur agent rekey — design + 6-milestone plan
```

(plus this docs commit + the M6.4 CLAUDE.md update.)

### mur-commander (`feat/agent-rekey-commander`)

```
f2121ab feat(commander): hourly grace sweep clears expired previous_pubkey (M6.2)
1927ea5 feat(engine): split-attestation detection with quarantine marker (M5.2) + bump mur-common to chain-verifier rev
b2e3c0c feat(cli): murc agent approve-rekey / reject-rekey (M4.2 CLI, FS-gated)
05695d8 feat(engine): murmur_bridge marks emergency rotations pending approval (M4.3)
64c7ce0 feat(engine): approve_emergency_rotation + reject_emergency_rotation (M4.2 engine)
8734e7a feat(engine): murmur_bridge applies rotation attestations from agent FS (M2.3)
b8d7d40 feat(engine): AgentRegistry::apply_rotation with signature + grace TTL (M2.2)
62cb09a feat(engine): RegisteredAgent rotation bookkeeping fields (M2.1)
```

## Open / non-blocking items

- **Hub-side attestation chain federation** — commanders currently apply rotations independently; multi-commander gossip via the mur-run hub lands as part of P1's hub work. Per spec § 9, the design treats commanders/hubs as distribution mechanisms, not trust anchors — attestations self-verify, so no inter-commander trust is required for the federated path.
- **Post-quantum** — schema has `algorithm` field; actual `ed25519+kyber768` hybrid signing waits for upstream Rust crate stability.
- **Hardware-backed keys (YubiKey / TPM / Secure Enclave)** — out of scope; can be added as algorithm variants later.
- **Key escrow / master recovery** — deliberately not supported per spec. Emergency rekey is the only recovery path.
- **mur-common dep bounce** — once mur cuts `v2.4.0`, bump commander's `mur-common` pin from `rev = "2b51b1e"` to `tag = "v2.4.0"`.
- **CI billing** — both PRs have CI-failed status due to GitHub Actions billing on the org. Local test/clippy/fmt all green.
