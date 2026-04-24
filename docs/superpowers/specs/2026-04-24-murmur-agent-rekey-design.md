# murmur `mur agent rekey` — Identity Keypair Rotation Design

**Status:** Spec v1 — 2026-04-24
**Resolves:** P0a.5 Open Question Q-B (spec `2026-04-23-murmur-fleet-architecture-design.md` § 13)
**Depends on:** P0a.5 (identity + Noise XK + commander bridge) — PRs [mur#29](https://github.com/mur-run/mur/pull/29) / [mur-commander#11](https://github.com/mur-run/mur-commander/pull/11)

## Problem

Each murmur agent owns a long-lived Ed25519 `identity.key` (stored 0600 on disk, derived
to X25519 as the Noise XK static secret). Its pubkey is published in `running.lock`,
`profile.yaml`, and the A2A Agent Card, and cached by the commander registry plus every
peer that has ever connected.

Over the life of the fleet, this key must be rotated:

| Scenario | Urgency |
|---|---|
| Private key leaked (committed to git, host compromised, backup stolen) | Immediate |
| Scheduled rotation for compliance (90 / 180 / 365 day policies) | Planned |
| Owner handoff (agent reassigned to another team) | On transfer |
| Host migration (laptop → VM → k8s) with trust boundary change | On migration |
| Trust revocation from a specific commander | On demand |
| Future crypto-agility (Ed25519 → post-quantum) | Long term |

Left unsolved, a compromised key stays trusted forever, and offline peers have no
reconciliation path.

## Design Principles

1. **UUID is stable, pubkey is rotatable** — `AgentProfile.id` (UUIDv7) never changes;
   only `identity.pubkey` rotates. Avoids libp2p's "new peer on rekey" trap.
2. **Attestation is the authority** — rotations carry an attestation signed by the
   outgoing key. Any party with the prior pubkey verifies independently; commanders
   and hubs are distributors, not trust anchors.
3. **Grace period overlap** — the prior pubkey stays valid for 30 days so offline
   peers reconcile without forced TOFU conflicts.
4. **No silent trust override** — if attestation signature fails, the commander
   refuses to update and raises an audit event.
5. **Two rotation paths** — normal (requires old key) vs emergency (old key
   unrecoverable; requires out-of-band admin approval on commander host).

## Schema Changes

### `mur-common::agent::IdentityConfig`

```rust
pub struct IdentityConfig {
    pub pubkey: String,
    pub owner: Option<String>,

    // P0a.5 rekey extensions (all #[serde(default)] — back-compat)
    #[serde(default = "default_algorithm")]
    pub algorithm: String,                    // "ed25519" (v1)
    #[serde(default)]
    pub key_version: u32,                     // monotonic; 0 = initial create
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_key: Option<String>,       // RFC3339 — current key creation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_pubkey: Option<String>,      // during grace
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_key_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_expires_at: Option<String>,     // RFC3339
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,           // RFC3339 — most recent rotation
}

fn default_algorithm() -> String { "ed25519".into() }

/// Algorithms the runtime can generate + verify.
pub const SUPPORTED_ALGORITHMS: &[&str] = &["ed25519"];
```

### `mur-common::identity::RotationAttestation`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationAttestation {
    pub schema: u32,                // = 1
    pub uuid: String,               // agent UUIDv7
    pub algorithm: String,          // "ed25519"
    pub old_pubkey: String,         // multibase base58btc
    pub new_pubkey: String,
    pub old_key_version: u32,
    pub new_key_version: u32,       // = old_key_version + 1 for non-emergency
    pub rotated_at: String,         // RFC3339 UTC
    pub reason: RotationReason,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub signature: String,          // multibase; empty for emergency
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationReason {
    Scheduled,
    SuspectCompromise,
    OwnerChange,
    Emergency,
}
```

The signature covers a canonical serialization of the attestation with the
`signature` field removed (JSON with sorted keys + no whitespace).

### `mur-commander::engine::a2a::discovery::RegisteredAgent`

```rust
pub struct RegisteredAgent {
    // existing P0a.5 fields...
    pub algorithm: Option<String>,
    pub key_version: u32,
    pub previous_pubkey: Option<String>,
    pub previous_key_version: Option<u32>,
    pub grace_expires_at: Option<DateTime<Utc>>,
    pub rotation_count: u32,
    pub last_rotation_at: Option<DateTime<Utc>>,
    /// Set if `apply_rotation` detected a conflict (split attestation).
    /// Agent is quarantined until admin clears.
    pub compromised_marker: Option<CompromisedMarker>,
}

pub struct CompromisedMarker {
    pub detected_at: DateTime<Utc>,
    pub reason: String,             // "split_attestation_v2_to_v3"
    pub conflicting_pubkeys: Vec<String>,
}
```

## CLI Surface

```bash
# Normal rotation (requires current identity.key on disk)
mur agent rekey <name> [--reason scheduled|suspect-compromise|owner-change]

# Emergency rotation (old key unrecoverable)
mur agent rekey <name> --emergency

# Inspect state
mur agent rekey-status <name>

# Commander admin approval for an emergency rekey
murc agent approve-rekey <uuid>
murc agent reject-rekey <uuid>  --reason "..."
```

## On-disk Layout After Rotation

```
~/.mur/agents/<name>/
├── identity.key                  # new private key, 0600
├── identity.pub                  # new multibase pubkey
├── identity.key.prev             # previous private key, 0600 — shredded at grace expiry
├── identity.pub.prev             # previous pubkey
├── identity.attestation.json     # latest attestation
├── rotations.jsonl               # append-only history, one attestation per line
└── profile.yaml                  # with extended IdentityConfig
```

## Flow — Normal Rotation

```
1. User runs: mur agent rekey <name>
2. CLI confirms interactively (shows current key_version, grace period, reason).
3. Generate new Ed25519 keypair in memory.
4. Build RotationAttestation { old_*, new_*, reason, timestamp }.
5. Sign attestation with current identity.key.
6. Atomically rotate on disk:
     mv identity.key     -> identity.key.prev
     mv identity.pub     -> identity.pub.prev
     write new identity.key (0600) + identity.pub
     write identity.attestation.json
     append to rotations.jsonl
7. Update profile.yaml:
     identity.previous_pubkey      = OLD_PUB
     identity.previous_key_version = OLD_VER
     identity.pubkey               = NEW_PUB
     identity.key_version          = OLD_VER + 1
     identity.rotated_at           = now
     identity.grace_expires_at     = now + 30d
     identity.created_at_key       = now
8. SIGTERM running runtime (if any); symlink supervisor restarts it.
9. New runtime:
     - Loads new identity.
     - Writes new running.lock (carries new pubkey).
     - Publishes updated Agent Card (pubkey + previous_pubkey).
10. Commander side (automatic):
     murmur_bridge sees running.lock MODIFY event.
     Reads identity.attestation.json alongside.
     Calls AgentRegistry::apply_rotation(uuid, attestation).
     - Verifies old_pubkey matches registry's cached pubkey.
     - Verifies old_key_version == registry.key_version.
     - Verifies attestation signature against old_pubkey.
     - On success: updates registry; TTL on previous_pubkey = 30d.
     - On failure: logs audit event, does NOT update, alerts operator.
11. Peer migration: lazy.
     - Peers with cached OLD pubkey fail Noise handshake.
     - Transport wrapper catches auth failure → re-fetches Agent Card → retries
       once with new pubkey (or previous_pubkey if still within grace).
     - Caches updated (pubkey, key_version).
```

## Flow — Emergency Rotation

```
1. User runs: mur agent rekey <name> --emergency
2. CLI prints big warning, requires typed "I UNDERSTAND" to proceed.
3. Generate new keypair; NO signature possible.
4. Write an unsigned attestation (signature = ""), reason = Emergency.
5. Rotate files as before (identity.key.prev is still written if old key file exists;
   may be absent if that's *why* emergency was triggered).
6. Update profile.yaml with key_version += 1 and an extra
   `identity.emergency_rekey_at` marker.
7. Restart runtime; new running.lock published.
8. Commander side:
     murmur_bridge sees running.lock MODIFY + unsigned attestation.
     AgentRegistry does NOT apply automatically.
     Marks agent state = PendingEmergencyApproval.
     Emits high-priority audit event: emergency_rekey_requested.
9. Out-of-band approval:
     Admin SSHs into commander host.
     Runs: murc agent approve-rekey <uuid>
     That CLI requires FS access to ~/.mur/commander/ — proves local host auth.
     On approval: registry updated; agent state cleared; peer migration proceeds.
10. Peers in the meantime: all handshakes fail until commander is approved and
    Agent Card reflects new pubkey. Fail-closed, not fail-open.
```

## Security Model

| Threat | Mitigation |
|---|---|
| Attacker steals `identity.key` | Legitimate user rotates → attestation chain advances past attacker. Attacker's stolen key becomes `previous_pubkey`, expires in 30d. |
| Attacker compromises one commander | Can delete registry entries but cannot forge attestations. Agent re-pushes on next `running.lock` update. |
| Attacker compromises agent host (full FS) | **Existential threat.** They can sign any rotation. Mitigation: emergency rekey path requires *another* host's FS (commander host). Ops can catch anomaly via audit events. |
| Split attestation (two rotations off same `key_version`) | `apply_rotation` fails when `old_key_version != registry.key_version`. Second rotation sees version mismatch → quarantine + alert. |
| Offline peer with stale pubkey | On handshake failure, peer refetches Agent Card (both `pubkey` and `previous_pubkey` served during grace). Handshake retried with current key. |
| MITM modifying pubkey in transit | Attestation signature verification fails → rotation rejected. |
| Post-quantum migration | `algorithm` field in schema + attestation lets us add `ed25519+kyber768` later with hybrid signatures. |

## Federation Architecture (P1+)

The spec mandates: **attestation is authority; commanders/hubs are distribution**.

**P0a.5+ (single commander, this spec)** — primary commander watches local FS via
`murmur_bridge`. `murc agent approve-rekey` requires local FS. No gossip.

**P1 hub integration** — commanders push attestation chains to `mur-run` hub; other
commanders pull on startup and on demand. Hub performs split detection across
commanders. No inter-commander trust needed — attestations self-verify.

**P2 direct commander-to-commander gossip** — optional fallback if hub is
unreachable. Uses A2A `identity/sync` method. Deferred until P1 experience
shows it's needed.

**Explicitly rejected**:
- Automatic inter-commander trust ("C1 said OK so it's OK").
- Silent pubkey replacement without attestation verification.
- Majority-vote quorum schemes (one compromised key can mint unlimited
  attestations at current version; majority vote doesn't help).

## Grace Period: 30 Days

Default `grace_expires_at = rotated_at + 30d`. Rationale:

- **Lambda / cron-triggered agents** may run only monthly; they need to catch up
  without breaking.
- **Laptops** that travel and miss cycles still reconcile without manual intervention.
- **Audit trail** of last-month's key is useful during incident investigation.
- **Attacker exposure** from keeping old key valid is bounded — the attacker can
  only do what the old key could do, and any attempt to rotate *from* the old key
  fails version check because current version has already moved on.

Configurable via commander config:

```toml
# ~/.mur/commander/config.toml
[identity]
grace_period_days = 30                       # 1..=90 allowed
emergency_rekey_requires_approval = true     # option (a): FS on commander host
max_key_age_days = 365                       # agents older get a startup nag
```

## Grace Expiry Cleanup

On every runtime startup and once daily:

1. Agent-side: if `grace_expires_at < now`:
   - `shred -u identity.key.prev` (Unix) / secure delete (Windows)
   - Remove `identity.pub.prev`
   - Clear `IdentityConfig.previous_pubkey`, `previous_key_version`,
     `grace_expires_at`, save profile.
   - Rewrite `running.lock` without `previous_pubkey`.

2. Commander-side: on the next watcher tick after agent's grace expiry:
   - `RegisteredAgent.previous_pubkey = None`
   - `previous_key_version = None`
   - `grace_expires_at = None`

## Audit Trail

`~/.mur/agents/<name>/rotations.jsonl` is append-only and contains the full
chain:

```json
{"schema":1,"uuid":"01JQX...","algorithm":"ed25519","old_pubkey":"","new_pubkey":"zK0","old_key_version":0,"new_key_version":0,"rotated_at":"2026-04-22T10:00:00Z","reason":"scheduled","signature":"","bootstrap":true}
{"schema":1,"uuid":"01JQX...","algorithm":"ed25519","old_pubkey":"zK0","new_pubkey":"zK1","old_key_version":0,"new_key_version":1,"rotated_at":"2026-06-01T10:00:00Z","reason":"scheduled","signature":"z..."}
{"schema":1,"uuid":"01JQX...","algorithm":"ed25519","old_pubkey":"zK1","new_pubkey":"zK2","old_key_version":1,"new_key_version":2,"rotated_at":"2026-09-01T10:00:00Z","reason":"suspect-compromise","signature":"z..."}
```

Line 0 is a bootstrap entry written at agent create time (no prior key, empty
signature, flagged with `bootstrap=true`). Every subsequent line carries the
attestation the commander verified.

Commander-side audit:

- `~/.mur/commander/rotations.jsonl` mirrors the chain with additional fields:
  `verified_at`, `source_host`, `verdict` (Applied | RejectedBadSig |
  RejectedVersionMismatch | QuarantinedSplit).

## Test Matrix

| Test | Scope |
|---|---|
| Bootstrap attestation on `mur agent create` | M1 |
| `mur agent rekey` generates attestation, verifies round-trip | M1 |
| `mur agent rekey` with wrong key_version in chain → reject | M1 |
| Attestation signature tamper → commander rejects | M2 |
| Split attestation (two rotations off same version) → quarantine | M5 |
| TcpConnector falls back to previous_pubkey during grace | M3 |
| Grace expiry shreds prev files + clears registry entry | M6 |
| Emergency rekey requires explicit approval | M4 |
| `approve-rekey` fails if caller lacks FS access | M4 |
| Legacy P0a profile (no identity block) gets key_version=0 on first create | M1 |
| 100 chained rotations still verify top-down | M5 |

## Files to Create / Modify

### mur-common

- **modify** `src/agent.rs` — extend `IdentityConfig`
- **modify** `src/identity.rs` — `RotationAttestation`, sign/verify helpers
- **new test** `tests/rotation_attestation.rs`

### mur-agent-runtime

- **modify** `src/supervisor.rs` — grace expiry cleanup at startup
- **modify** `src/transport/tcp.rs` — `TcpConnector::dial_with_fallback` accepting
  `[primary_pubkey, ...fallback_pubkeys]`
- **modify** `src/protocol/methods/card.rs` — include `previous_pubkey` in card

### mur-core

- **new** `src/cmd/agent_rekey.rs` — `rekey`, `rekey_status` subcommands
- **modify** `src/cmd/agent.rs` — bootstrap attestation on `create`

### mur-commander

- **modify** `engine::a2a::discovery::RegisteredAgent` — new fields
- **modify** `engine::a2a::discovery::AgentRegistry` — `apply_rotation`,
  `approve_emergency_rotation`, split-detection path
- **modify** `engine::remote::murmur_bridge` — read attestation alongside
  running.lock; call `apply_rotation`; handle PendingEmergencyApproval
- **new** `murc agent approve-rekey` / `reject-rekey` subcommands
- **modify** `daemon::config` — `[identity]` section

## Open Non-blocking Items

- **Post-quantum**: schema ready; actual `ed25519+kyber768` impl deferred until
  upstream Rust crates stabilize and NIST finalizes parameters.
- **Hardware-backed keys**: future work (YubiKey / TPM / Secure Enclave for
  `identity.key`) is out of scope here; can be added as an `algorithm` variant
  like `ed25519-yubikey-pin`.
- **Key escrow / recovery**: deliberately not supported. If an agent's key is
  truly lost, emergency rekey is the only path. We will not ship a "master key
  that can sign any agent's rotation" mechanism — that would undermine the
  entire trust model.
