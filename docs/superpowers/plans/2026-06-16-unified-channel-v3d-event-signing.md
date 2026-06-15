# Unified Channel v3d — Per-Event Signing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Implements the v3d signing primitive from `2026-06-16-unified-channel-v3-design.md` §7 v3d. Builds on v3a (the reserved `ChannelEvent.sig`/`key_version`) + v3c (the HITL gate that consumes a `HitlResponse`). **The full A2 "peer-writes-own" runtime rework is carved out as v3d-2** (see §scope) — this plan delivers the signing/verify primitive + makes the channel `HitlResponse` authority-bearing, which is what v4b's phone HITL needs.

**Goal:** sign every channel event with the writer's Ed25519 identity at append, and verify-on-fold against the writer's pubkey (resolved through the rotation chain at the event's `key_version`). This closes the v3 trust-model residual ("any local process can append forged events into the shared log") and makes a channel `HitlResponse` **authority-bearing** (a forged response fails verification), unblocking authoritative high-risk approval from the Hub/phone.

**Architecture:** the **writer signs** (A1 sole-writer): the process that appends an event signs the canonical sign-input with the channel's router/owner agent identity (`~/.mur/agents/<router>/identity.key`). The sign-input is the canonical JSON of `{v, channel_id, actor, kind, payload, idempotency_key}` — **excluding the store-assigned `seq` and `ts`** so the caller can sign *before* `append_event` assigns them (this corrects the v3a doc-comment, which wrongly said `seq||ts||actor||kind||payload`). Verify-on-fold recomputes that input and checks `sig` against the writer's pubkey at `key_version` (via `verify_chain` over `rotations.jsonl`). Verification is **advisory during a migration window** (unsigned/legacy events pass with a logged warning), then **enforced** for trust-critical reads (the HITL gate first). `mur-channel` already depends on `mur-common` (where `AgentIdentity` lives), so the crypto stays in `mur-channel` with no new crate edges.

**Tech Stack:** Rust — `mur-common` (`AgentIdentity`/`verify_chain`, already shipped), `mur-channel` (new `sign` module + signed append + verify-on-fold), `mur-core` (wire the writer-signs into the executor/CLI/mobile append paths + verify into the HITL gate). No `mur-agent-runtime` change in this plan (that is v3d-2).

**Scope (this plan = v3d-1, the primitive):**
- **In:** sign-input canonicalization (corrected); `sign_event`/`verify_event_sig`; signed `append`; rotation-chain pubkey resolution; `verify_log` fold helper; writer-signs wired into the local append paths; verify enforced in the v3c HITL gate; migration flag (advisory→enforce).
- **Out (→ v3d-2, a separate plan):** A2 "peer-writes-own" — adding `mur-channel` to `mur-agent-runtime`, a `channel/delegate` dispatcher method, and specialists appending+signing their own events. Until v3d-2, delegated-agent events remain **concierge-written + concierge-signed** (A1); the sig proves "the trusted concierge recorded this," which is the anti-forgery property we need now.
- **Out:** the `task_runner.rs` pre-execution reorder + cached-tail `next_seq` (separate perf/HITL items the v3 spec also parked under v3d).

**Scope guardrails:**
- Sign-input **excludes `seq`/`ts`** (caller signs before append). **Correct the v3a doc-comment.**
- Writer signs with the channel's **router/owner agent identity** (A1); verify against that writer's pubkey, NOT the `actor` field's (in A1 the actor may be a specialist the concierge is attributing).
- Migration: unsigned events are **accepted with a warning** while `MUR_CHANNEL_REQUIRE_SIG` (config) is off; flip to enforce once the log is signed end-to-end.
- A forged/invalid-sig event on a trust-critical read (HITL) is **dropped fail-closed**.

**Key facts locked during exploration (do not re-derive):**
- `AgentIdentity` (`mur-common/src/identity.rs`): `load(dir)->Result<Self>` (`:71`), `sign_bytes(&[u8])->[u8;64]` (`:108`), `verifying_key_bytes()->[u8;32]` (`:117`), `pubkey_text()->String` (multibase base58btc, `:121`), `decode_pubkey(text)->[u8;32]` (`:149`). Keys at `<agent_home>/identity.{key,pub}`.
- Rotation: `RotationAttestation` + `verify_chain(&[RotationAttestation], ChainOptions)->Result<ChainOutcome{head_key_version,head_pubkey,length},ChainError>` (`:416`); chain stored append-only at `<agent_home>/rotations.jsonl`; `profile.identity.key_version` (`agent.rs IdentityConfig`) is the current version.
- `ChannelEvent.sig: Option<String>` + `key_version: Option<u32>` (`channel.rs:150,154`) — reserved, currently always `None`; `append_event` (`store.rs:73`) hardcodes them `None`. The doc-comment's sign-input (`seq||ts||actor||kind||payload`) is **wrong** and must be corrected.
- `load_events` (`store.rs:56`) silently skips unparseable lines. Consumers: Hub `work.rs`, CLI `persist.rs`, mobile `mobile.rs::channel_query`, the v3c `hitl/gate.rs` (the trust-critical one), executor `dag.rs` resume cursor.
- v3c HITL gate (`mur-core/src/hitl/gate.rs`): `wait_for_response` finds a `HitlResponse` event by `hitl_id` and trusts `allow`. This is where authority must become signature-checked.
- `ed25519_dalek` is already a dep (via `mur-common`); `multibase` is used for pubkey/sig encoding.

---

## File Structure

**Created:**
- `mur-channel/src/sign.rs` — `sign_input`, `sign_event`, `verify_event_sig`, `resolve_writer_pubkey`, `verify_log`.

**Modified:**
- `mur-common/src/channel.rs` — correct the `sig` doc-comment (exclude seq/ts; name the canonical input).
- `mur-channel/src/store.rs` — `append_event` accepts optional `sig`/`key_version`.
- `mur-channel/src/service.rs` — `append_signed(...)` (sign then append) + `verify_events(...)`.
- `mur-channel/src/lib.rs` — `pub mod sign;`.
- `mur-core/src/...` (executor `dag.rs`, CLI `persist.rs`, `mobile.rs`) — route local appends through `append_signed` with the router identity (behind the migration flag).
- `mur-core/src/hitl/gate.rs` — verify the `HitlResponse` signature (authority-bearing).

---

## Task 1: Canonical sign-input (+ correct the doc-comment)

**Files:**
- Create: `mur-channel/src/sign.rs`; Modify: `mur-common/src/channel.rs:148-154`, `mur-channel/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-channel/src/sign.rs`:

```rust
//! Per-event Ed25519 signing (v3d). The WRITER signs the canonical sign-input —
//! `{v, channel_id, actor, kind, payload, idempotency_key}` — EXCLUDING the
//! store-assigned `seq` and `ts` (the signer does not know `seq`; the store
//! restamps `ts` under the append lock), so the caller can sign BEFORE append.
//! Verify-on-fold recomputes this input and checks `sig` against the writer's
//! pubkey at `key_version` (resolved via the rotation chain).

use mur_common::channel::{ChannelActor, EventKind};

/// Canonicalization version — bump if the sign-input shape changes so an old
/// signature is never silently checked against a new canonicalization.
pub const SIG_INPUT_VERSION: u32 = 1;

/// Canonical bytes signed for an event. `serde_json` sorts object keys (no
/// preserve_order), so this is deterministic for a given input.
pub fn sign_input(
    channel_id: &str,
    actor: &ChannelActor,
    kind: EventKind,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
) -> Vec<u8> {
    let canon = serde_json::json!({
        "v": SIG_INPUT_VERSION,
        "channel_id": channel_id,
        "actor": actor,
        "kind": kind,
        "payload": payload,
        "idempotency_key": idempotency_key,
    });
    serde_json::to_vec(&canon).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_input_excludes_seq_ts_and_is_stable() {
        let actor = ChannelActor::Agent { id: "qa".into() };
        let p = serde_json::json!({ "text": "hi" });
        let a = sign_input("c1", &actor, EventKind::Message, &p, Some("k1"));
        let b = sign_input("c1", &actor, EventKind::Message, &p, Some("k1"));
        assert_eq!(a, b, "deterministic");
        // Different channel / payload / key → different input.
        assert_ne!(a, sign_input("c2", &actor, EventKind::Message, &p, Some("k1")));
        assert_ne!(a, sign_input("c1", &actor, EventKind::Message, &serde_json::json!({"text":"yo"}), Some("k1")));
        assert_ne!(a, sign_input("c1", &actor, EventKind::Message, &p, None));
        // It must NOT contain seq/ts tokens.
        let s = String::from_utf8_lossy(&a);
        assert!(!s.contains("\"seq\""));
        assert!(!s.contains("\"ts\""));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-channel sign::tests::sign_input_excludes_seq_ts_and_is_stable` — Expected: FAIL (module not declared).

- [ ] **Step 3: Declare the module + correct the doc-comment**

In `mur-channel/src/lib.rs` add `pub mod sign;`. In `mur-common/src/channel.rs`, replace the `sig` field doc-comment (the wrong `seq||ts||actor||kind||payload`) with:

```rust
    /// Detached Ed25519 signature (multibase) by the channel's WRITER over the
    /// canonical sign-input `{v, channel_id, actor, kind, payload,
    /// idempotency_key}` — EXCLUDING the store-assigned `seq`/`ts` (see
    /// `mur-channel` `sign::sign_input`). `None` for legacy/unsigned events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-channel sign::` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/sign.rs mur-channel/src/lib.rs mur-common/src/channel.rs
git commit -m "feat(channel): canonical event sign-input (excludes seq/ts); fix v3a doc (v3d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: `sign_event` + `verify_event_sig`

**Files:**
- Modify: `mur-channel/src/sign.rs`

- [ ] **Step 1: Write the failing test**

Append to `sign.rs` tests:

```rust
    use mur_common::identity::AgentIdentity;
    use tempfile::TempDir;

    #[test]
    fn sign_then_verify_roundtrips_and_rejects_tamper() {
        let tmp = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        id.save(tmp.path()).unwrap();
        let actor = ChannelActor::Agent { id: "mur".into() };
        let payload = serde_json::json!({ "text": "approved" });

        let sig = sign_event(&id, "c1", &actor, EventKind::HitlResponse, &payload, Some("k1"));
        let pub_bytes = id.verifying_key_bytes();
        assert!(verify_event_sig("c1", &actor, EventKind::HitlResponse, &payload, Some("k1"), &sig, &pub_bytes));
        // Tampered payload → fails.
        let tampered = serde_json::json!({ "text": "DENIED" });
        assert!(!verify_event_sig("c1", &actor, EventKind::HitlResponse, &tampered, Some("k1"), &sig, &pub_bytes));
        // Wrong key → fails.
        let other = AgentIdentity::generate();
        assert!(!verify_event_sig("c1", &actor, EventKind::HitlResponse, &payload, Some("k1"), &sig, &other.verifying_key_bytes()));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-channel sign::tests::sign_then_verify_roundtrips_and_rejects_tamper` — Expected: FAIL (functions not found).

- [ ] **Step 3: Implement sign/verify**

Add to `sign.rs`:

```rust
use mur_common::identity::AgentIdentity;

/// Sign an event's canonical input with `identity`; returns the multibase sig.
pub fn sign_event(
    identity: &AgentIdentity,
    channel_id: &str,
    actor: &ChannelActor,
    kind: EventKind,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
) -> String {
    let input = sign_input(channel_id, actor, kind, payload, idempotency_key);
    let sig = identity.sign_bytes(&input);
    multibase::encode(multibase::Base::Base58Btc, sig)
}

/// Verify a multibase signature over an event's canonical input against a raw
/// Ed25519 pubkey. Returns false on any decode/verify failure (fail-closed).
pub fn verify_event_sig(
    channel_id: &str,
    actor: &ChannelActor,
    kind: EventKind,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
    sig_multibase: &str,
    pubkey: &[u8; 32],
) -> bool {
    let Ok((_b, sig_bytes)) = multibase::decode(sig_multibase) else { return false };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.as_slice().try_into() else { return false };
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(pubkey) else { return false };
    let input = sign_input(channel_id, actor, kind, payload, idempotency_key);
    use ed25519_dalek::Verifier;
    vk.verify(&input, &sig).is_ok()
}
```

(Add `ed25519_dalek` + `multibase` to `mur-channel/Cargo.toml` if not already present — `grep -n "ed25519\|multibase" mur-channel/Cargo.toml`; both are transitively available via `mur-common`, add explicit deps if the crate doesn't see them.)

- [ ] **Step 4: Run + commit**

Run: `cargo test -p mur-channel sign::` — Expected: PASS.

```bash
git add mur-channel/src/sign.rs mur-channel/Cargo.toml
git commit -m "feat(channel): sign_event/verify_event_sig (Ed25519 over sign-input) (v3d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: Signed append

**Files:**
- Modify: `mur-channel/src/store.rs` (`append_event` gains sig/key_version), `mur-channel/src/service.rs` (`append_signed`)

- [ ] **Step 1: Write the failing test**

Add to `service.rs` tests:

```rust
    #[test]
    fn append_signed_stores_verifiable_sig() {
        let tmp = TempDir::new().unwrap();
        let id = mur_common::identity::AgentIdentity::generate();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("signed").unwrap();
        let ev = svc.append_signed(
            &ch.id, &id, 0,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            serde_json::json!({ "text": "hi" }),
            None,
        ).unwrap();
        assert!(ev.sig.is_some());
        assert_eq!(ev.key_version, Some(0));
        // The stored sig verifies against the signer's pubkey.
        let loaded = svc.load_events(&ch.id).unwrap();
        let e = &loaded[0];
        assert!(crate::sign::verify_event_sig(
            &ch.id, &e.actor, e.kind, &e.payload, e.idempotency_key.as_deref(),
            e.sig.as_ref().unwrap(), &id.verifying_key_bytes()
        ));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-channel service::tests::append_signed_stores_verifiable_sig` — Expected: FAIL (no `append_signed`).

- [ ] **Step 3: Extend `append_event` + add `append_signed`**

In `store.rs`, change `append_event` to accept the two optional fields (add params at the end, default `None` from existing callers). Replace its signature + the `ChannelEvent` literal:

```rust
    pub fn append_event(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        sig: Option<String>,
        key_version: Option<u32>,
    ) -> Result<ChannelEvent> {
        // … unchanged lock + dedup + next_seq …
        let ev = ChannelEvent { seq: next_seq, ts: Utc::now(), actor, kind, payload, idempotency_key, sig, key_version };
        // … unchanged write + unlock …
    }
```

Update the existing `append_event` call sites (in `service.rs::append`) to pass `None, None`. (`grep -n "append_event(" mur-channel/src` to find them — only `service.rs::append` and tests.)

In `service.rs`, add:

```rust
    /// Sign an event with `identity` (key_version `kv`) and append it. Used by
    /// the channel's writer (the router/owner) so the log is forgery-resistant.
    pub fn append_signed(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let sig = crate::sign::sign_event(identity, channel_id, &actor, kind, &payload, idempotency_key.as_deref());
        let ev = self.store.append_event(
            channel_id, actor, kind, payload, idempotency_key, Some(sig), Some(kv),
        )?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(ev)
    }
```

(Make `append` delegate to `append_event(..., None, None)` for the unsigned path; both stay.)

- [ ] **Step 4: Run + commit**

Run: `cargo test -p mur-channel` — Expected: PASS (new + existing; existing callers now pass `None,None`).

```bash
git add mur-channel/src/store.rs mur-channel/src/service.rs
git commit -m "feat(channel): append_signed — sign-on-append with writer identity (v3d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: Writer-pubkey resolution + verify-on-fold

**Files:**
- Modify: `mur-channel/src/sign.rs`

- [ ] **Step 1: Write the failing test**

Append to `sign.rs` tests:

```rust
    #[test]
    fn verify_log_drops_forged_keeps_valid_and_tolerates_legacy() {
        let tmp = TempDir::new().unwrap();
        let writer = AgentIdentity::generate();
        let svc_home = tmp.path();
        // Build three events: a valid signed one, a forged (wrong-key) one, and a
        // legacy unsigned one.
        let actor = ChannelActor::Agent { id: "mur".into() };
        let p = serde_json::json!({ "text": "ok" });
        let good_sig = sign_event(&writer, "c1", &actor, EventKind::Message, &p, None);
        let forged_sig = sign_event(&AgentIdentity::generate(), "c1", &actor, EventKind::Message, &p, None);
        let mk = |sig: Option<String>| mur_common::channel::ChannelEvent {
            seq: 0, ts: chrono::Utc::now(), actor: actor.clone(),
            kind: EventKind::Message, payload: p.clone(), idempotency_key: None,
            sig, key_version: sig_kv(),
        };
        let _ = svc_home;
        let pubkey = writer.verifying_key_bytes();
        // valid
        assert!(verify_one("c1", &mk(Some(good_sig)), &pubkey, false));
        // forged → fails (enforce or not, a present-but-bad sig is always rejected)
        assert!(!verify_one("c1", &mk(Some(forged_sig)), &pubkey, false));
        // legacy unsigned: tolerated when !require_sig, rejected when require_sig
        assert!(verify_one("c1", &mk(None), &pubkey, false));
        assert!(!verify_one("c1", &mk(None), &pubkey, true));
    }
    fn sig_kv() -> Option<u32> { Some(0) }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-channel sign::tests::verify_log_drops_forged_keeps_valid_and_tolerates_legacy` — Expected: FAIL (`verify_one` not found).

- [ ] **Step 3: Implement resolution + verify**

Add to `sign.rs`:

```rust
use mur_common::channel::ChannelEvent;
use std::path::Path;

/// Verify a single event against a known writer pubkey. A present sig must be
/// valid (always). A missing sig is tolerated only when `!require_sig`.
pub fn verify_one(channel_id: &str, ev: &ChannelEvent, writer_pubkey: &[u8; 32], require_sig: bool) -> bool {
    match ev.sig.as_deref() {
        Some(sig) => verify_event_sig(
            channel_id, &ev.actor, ev.kind, &ev.payload, ev.idempotency_key.as_deref(), sig, writer_pubkey,
        ),
        None => !require_sig,
    }
}

/// Resolve the writer's pubkey for a given `key_version` by folding the agent's
/// rotation chain (`<agent_home>/rotations.jsonl`). Falls back to the current
/// `identity.pub` when no chain / version match (single-host bootstrap).
pub fn resolve_writer_pubkey(agent_home: &Path, key_version: Option<u32>) -> Option<[u8; 32]> {
    use mur_common::identity::{decode_pubkey, verify_chain, ChainOptions, RotationAttestation};
    let chain_path = agent_home.join("rotations.jsonl");
    if let Ok(text) = std::fs::read_to_string(&chain_path) {
        let chain: Vec<RotationAttestation> = text
            .lines().filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if let Ok(outcome) = verify_chain(&chain, ChainOptions { allow_emergency: false }) {
            // For the head version use head_pubkey; for older versions, find the
            // attestation whose new_key_version matches.
            if key_version == Some(outcome.head_key_version) || key_version.is_none() {
                if let Ok(b) = decode_pubkey(&outcome.head_pubkey) { return Some(b); }
            }
            if let Some(kv) = key_version {
                if let Some(att) = chain.iter().find(|a| a.new_key_version == kv) {
                    if let Ok(b) = decode_pubkey(&att.new_pubkey) { return Some(b); }
                }
            }
        }
    }
    // Fallback: current identity.pub.
    let txt = std::fs::read_to_string(agent_home.join("identity.pub")).ok()?;
    decode_pubkey(txt.trim()).ok()
}

/// Verify-on-fold: filter a log to the events that pass verification against the
/// channel's writer. Forged (bad-sig) events are dropped + logged; unsigned
/// events pass only when `!require_sig`.
pub fn verify_log(channel_id: &str, events: Vec<ChannelEvent>, writer_pubkey: &[u8; 32], require_sig: bool) -> Vec<ChannelEvent> {
    events.into_iter().filter(|ev| {
        let ok = verify_one(channel_id, ev, writer_pubkey, require_sig);
        if !ok { tracing::warn!(channel = channel_id, seq = ev.seq, "dropping unverifiable channel event"); }
        ok
    }).collect()
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p mur-channel sign::` — Expected: PASS.

```bash
git add mur-channel/src/sign.rs
git commit -m "feat(channel): verify-on-fold + rotation-chain pubkey resolution (v3d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: Wire writer-signs + verify the HITL response (authority)

**Files:**
- Modify: `mur-core/src/hitl/gate.rs`; the local append writers (`executor/dag.rs`, `cmd/agent/cli/persist.rs`, `mobile.rs`); a config flag.

- [ ] **Step 1: Writer-signs on the local append paths (behind the migration flag)**

Add a `mur-core` helper that resolves the channel's router/owner identity and signs:

```rust
// mur-core/src/channel_writer.rs (new): load the router agent's identity once and
// append signed. The router is the channel owner agent ("mur" for the concierge);
// for a workflow channel the executing agent. Falls back to unsigned append if the
// identity is unavailable (migration-safe).
pub fn append_as_writer(
    svc: &mur_channel::ChannelService, home: &std::path::Path, channel_id: &str, router_agent: &str,
    actor: mur_common::channel::ChannelActor, kind: mur_common::channel::EventKind,
    payload: serde_json::Value, idem: Option<String>,
) -> anyhow::Result<mur_common::channel::ChannelEvent> {
    let agent_home = home.join("agents").join(router_agent);
    if let Ok(id) = mur_common::identity::AgentIdentity::load(&agent_home) {
        let kv = read_key_version(&agent_home).unwrap_or(0);
        return svc.append_signed(channel_id, &id, kv, actor, kind, payload, idem);
    }
    svc.append(channel_id, actor, kind, payload, idem) // unsigned fallback
}
```

Route the existing local appends (executor `dag.rs` step events + delegation, CLI `persist.rs`, `mobile.rs::persist_mobile_exchange`, the v3c gate's `HitlRequest`/`HitlResponse`) through `append_as_writer` with the channel's router agent. (Mechanical: replace `svc.append(...)`/`svc.append_message(...)` at the writer sites; reads are unchanged.)

- [ ] **Step 2: Verify the `HitlResponse` signature in the gate**

In `mur-core/src/hitl/gate.rs::wait_for_response`, when a matching `HitlResponse` is found, verify its sig before trusting `allow` (fail-closed when `MUR_CHANNEL_REQUIRE_SIG` is on). Resolve the approver pubkey: for a phone approval, the paired phone pubkey; for a local CLI/Hub approval, the local human/router. v3d-1 verifies against the **channel writer** (the router that wrote it); a phone-originated response is the v4b/v4c + paired-pubkey path (note it):

```rust
            // Authority check (v3d): a present sig must verify against the writer;
            // unsigned tolerated only while require_sig is off (migration).
            let require = std::env::var("MUR_CHANNEL_REQUIRE_SIG").is_ok();
            let writer_pub = mur_channel::sign::resolve_writer_pubkey(&home.join("agents").join("mur"), resp.key_version);
            if let Some(pk) = writer_pub {
                if !mur_channel::sign::verify_one(channel_id, resp, &pk, require) {
                    tracing::warn!("HitlResponse failed signature verification — ignoring");
                    continue; // keep waiting; a forged response cannot release the gate
                }
            }
```

> Note: in v3d-1 the writer (concierge/router) signs the `HitlResponse` it records on the human's behalf (local trust). Phone-originated authoritative responses (signed by the paired phone key) are wired when v4c adds the phone write path — the gate's resolver is extended to also try the paired-pubkey set then.

- [ ] **Step 3: Build + test**

Run:
```bash
cargo build -p mur-core
cargo nextest run -p mur-channel -p mur-core
```
Expected: green. With `MUR_CHANNEL_REQUIRE_SIG` unset, legacy/unsigned logs still work (migration-safe).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/channel_writer.rs mur-core/src/hitl/gate.rs mur-core/src/executor/dag.rs mur-core/src/cmd/agent/cli/persist.rs mur-core/src/mobile.rs mur-core/src/lib.rs
git commit -m "feat(hitl): writer-signs channel events; gate verifies HitlResponse (v3d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 6: Quality gates + docs + flag rollout

- [ ] **Step 1: Gates**

```bash
cargo fmt && cargo fmt --check
cargo clippy -p mur-common -p mur-channel -p mur-core -- -D warnings
cargo nextest run -p mur-common -p mur-channel -p mur-core
```

- [ ] **Step 2: Migration check (advisory → enforce)**

```bash
# 1. With sig OFF (default), exercise a workflow/chat → events now carry a sig.
mur agent cli mur   # send a turn; then:
tail -2 ~/.mur/channels/*/events.jsonl   # expect "sig":"z…","key_version":0
# 2. Confirm verify-on-fold tolerates a pre-v3d (unsigned) channel without dropping turns.
# 3. Flip enforce and confirm a hand-forged event is dropped:
MUR_CHANNEL_REQUIRE_SIG=1 mur internals reindex   # forged/unsigned Agent lines dropped+warned
```

- [ ] **Step 3: Docs + memory**

- `CLAUDE.md`: note channel events are Ed25519-signed by the writer (v3d); `MUR_CHANNEL_REQUIRE_SIG` enforces verify-on-fold. Sign-input excludes seq/ts.
- Memory: v3d-1 (signing primitive + verify + HITL authority) done on `feat/unified-channel-v3b`; **v3d-2 (A2 peer-writes-own: runtime `mur-channel` dep + `channel/delegate` method + specialists sign their own events) is the remaining piece**, plus the `task_runner` pre-exec reorder + cached-tail `next_seq`.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: per-event channel signing + MUR_CHANNEL_REQUIRE_SIG (v3d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (`2026-06-16-unified-channel-v3-design.md` §7 v3d):**
- "sign-on-append via AgentIdentity::sign_bytes over {channel_id,actor,kind,payload,idempotency_key} EXCLUDING seq/ts" → Tasks 1-3; **corrects the v3a doc-comment** that wrongly included seq/ts. ✓
- "verify-on-fold against identity.pub resolved by key_version through verify_chain; drop forged/unsigned at fold with logged anomaly" → Task 4 (`resolve_writer_pubkey` + `verify_log`/`verify_one`). ✓
- "unlocks authority-bearing channel HitlResponse" → Task 5 (gate verifies the response sig). ✓ (phone-paired-key authority noted as the v4c wire-up.)
- "unlocks A2 peer-writes-own" → **explicitly carved out to v3d-2** (runtime `mur-channel` dep + `channel/delegate` method + specialist signs). v3d-1 keeps A1 (writer/concierge signs), which already delivers the anti-forgery property. Flagged. ✓
- "task_runner reorder + cached-tail next_seq" → out of scope (parked), per §scope. ✓

**2. Placeholder scan:** No "TBD"/"handle later". The crypto core (sign-input, sign/verify, signed append, verify-on-fold) is complete code. Task 5's writer-site rewiring is mechanical ("replace `svc.append` at the writer sites") with the helper given; a `grep` locates the sites. The A2 rework is a named deferral (v3d-2), not a placeholder.

**3. Type consistency:**
- `sign_input(channel_id, actor, kind, payload, idempotency_key)` (Task 1) reused by `sign_event`/`verify_event_sig` (Task 2) and `verify_one`/`verify_log` (Task 4) with identical argument order.
- `append_event` gains `sig: Option<String>, key_version: Option<u32>` (Task 3) — all existing callers updated to pass `None, None`; `append_signed` passes `Some(sig), Some(kv)`.
- `verify_event_sig(..., sig_multibase: &str, pubkey: &[u8;32]) -> bool` and `resolve_writer_pubkey(agent_home, key_version) -> Option<[u8;32]>` compose in `verify_one`/the gate (Task 5).
- Pubkey is `[u8;32]` everywhere (`verifying_key_bytes` / `decode_pubkey`); sig is multibase `String`.

**4. Scope check:** v3d-1 = the signing primitive + verify + HITL authority, in `mur-common`/`mur-channel`/`mur-core` (no runtime change). The crypto core is fully unit-tested; the writer-rewiring + gate verify are build/integration-verified with a migration flag (advisory→enforce) so it ships without breaking pre-v3d logs. A2 peer-writes-own (the runtime-touching half) is a named follow-on (v3d-2). Focused. ✓

No gaps found.
