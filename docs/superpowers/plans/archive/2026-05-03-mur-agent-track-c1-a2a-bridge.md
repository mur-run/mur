# Track C1 — A2A Bridge Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Land the *foundation pattern* for chat-platform bridges — every bridge is a **dumb, LLM-less mur agent** that signs every outbound A2A envelope, dedupes platform messages, advances offsets only after ACK, and surfaces heartbeat via `running.lock` mtime. No concrete platform (Telegram / Slack / Discord) is shipped here — only the plumbing and a stub bridge for E2E.

**Architecture:** Three crate touch-points: (1) `mur-common::bridge::*` adds frozen schema for `BridgeRouteConfig`, `SignedEnvelope`, and `TrustedPeer`; (2) we extend `Entitlements` with `llm: LlmEntitlement { mode: off|allowed }` so a bridge profile is *unable* to call an LLM by construction (B0 rule 2 already enforces network restrictions); (3) `mur-agent-runtime` gains a `bridge::` module with `DedupeStore` (sled), `AckTracker`, `BridgeBeacon` (30 s `bridge.alive` telemetry), and a peer-verifier that rejects unsigned/wrong-key envelopes regardless of transport. CLI gains `mur agent companion connector add --platform stub` to scaffold a bridge agent dir.

**Tech Stack:** Rust 2024, `mur-common` schema crate (serde_yaml_ng + serde_json + ed25519-dalek + multibase via existing `identity` module), `sled` 0.34 (NEW workspace dep — disk-backed kv), `mur-agent-runtime` Tokio supervisor, existing `TelemetryWriter`, `LockFile`, and `GrantStore` infrastructure.

**Predecessors on main:**
- M7 B0 rule 2 (`mur-common::permissions::GrantStore`) — first-use AskUser flow for `Restricted` outbound network. Default-deny posture for bridges.
- `HOOK_SCHEMA_VERSION = 2` (M7.6) — unchanged here.
- `mur-common::a2a::{Message, JsonRpcRequest, JsonRpcResponse}` — existing envelope types (P0a).
- `mur-common::identity::{AgentIdentity, encode_pubkey, decode_pubkey}` — Ed25519 sign/verify (P0a.5 + P0a.6 rekey).
- `mur-common::lock_file::classify(&Path)` + `mur-agent-runtime::lock_file` — `running.lock` infra.

---

## File Structure

### Created

| Path | Responsibility |
|---|---|
| `mur-common/src/bridge/mod.rs` | Public `bridge` module re-exports |
| `mur-common/src/bridge/routes.rs` | `BridgeRouteConfig`, `RouteEntry`, `RouteMatch`, `InboundMessage`, `Resolution` |
| `mur-common/src/bridge/envelope.rs` | `SignedEnvelope`, `EnvelopeError`, `sign_payload`, `verify_envelope_with_pubkey` |
| `mur-common/src/bridge/peer.rs` | `TrustedPeer { pubkey_multibase, name, key_version }` |
| `mur-common/src/bridge/llm_entitlement.rs` | `LlmEntitlement { mode: LlmMode }` |
| `mur-agent-runtime/src/bridge/mod.rs` | Module entry + module-level docs |
| `mur-agent-runtime/src/bridge/dedupe.rs` | `DedupeStore` (sled, 7-day TTL) |
| `mur-agent-runtime/src/bridge/ack.rs` | `AckTracker<T>` |
| `mur-agent-runtime/src/bridge/beacon.rs` | `BridgeBeacon` + `bridge_status_for_peer` |
| `mur-agent-runtime/src/bridge/verify.rs` | `verify_inbound_envelope` |
| `mur-core/src/cmd/agent_companion/connector.rs` | `connector add --platform stub` scaffold |
| `mur-agent-runtime/tests/bridge_*.rs` | 5 integration tests (signing, dedupe, ack, beacon, llm-off) |
| `mur-agent-runtime/tests/bridge_roundtrip.rs` | Stub-bridge full-loop |
| `mur-core/tests/connector_add_stub.rs` | CLI snapshot test |
| `scripts/e2e/c1-bridge-roundtrip.sh` | E2E shell harness |
| `docs/cookbook/c1-a2a-bridge.md` | Pattern explainer |

### Modified

| Path | Change |
|---|---|
| `Cargo.toml` (root) | Add `sled = "0.34"` to `[workspace.dependencies]` |
| `mur-common/src/lib.rs` | `pub mod bridge;` + re-export `LlmEntitlement`, `LlmMode` |
| `mur-common/src/agent.rs` | Add `pub trusted_peers: Vec<TrustedPeer>` to `AgentProfile`; add `pub llm: LlmEntitlement` to `Entitlements` (both `#[serde(default)]`) |
| `mur-common/Cargo.toml` | (verify `serde_bytes` present; add if missing) |
| `mur-agent-runtime/Cargo.toml` | `sled.workspace = true` |
| `mur-agent-runtime/src/lib.rs` | `pub mod bridge;` |
| `mur-agent-runtime/src/supervisor.rs` | Spawn `BridgeBeacon` when `llm.mode == Off` |
| `mur-core/src/cmd/agent_companion.rs` | Extend `CompanionCmd` with `Connector` |
| `mur-core/src/cmd/doctor.rs` | Add `bridges:` section + `collect_bridge_statuses` |
| `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` | Tick §5.1 / §5.2 / §5.3 footer |
| `CLAUDE.md` | One-line note about `entitlements.llm.mode` |

---

## Milestone Map (8 milestones, 1 PR per milestone in cascade)

| # | Milestone | Spec | Tasks |
|---|---|---|---|
| M-c1.0 | LLM entitlement (`llm.mode = off`) | §5.1 | 3 |
| M-c1.1 | `routes.yaml` schema + precedence | §5.2 | 4 |
| M-c1.2 | sled-backed `DedupeStore` + 7-day TTL | §5.3 | 4 |
| M-c1.3 | `SignedEnvelope` + `TrustedPeer` + verify | §5.3 | 5 |
| M-c1.4 | Heartbeat + degraded surface in `doctor` | §5.3 | 4 |
| M-c1.5 | `AckTracker` (advance only on 2xx) | §5.3 | 3 |
| M-c1.6 | `connector add --platform stub` CLI | §5.1 | 3 |
| M-c1.7 | E2E + cookbook + spec tick | §5.1 | 4 |

**Total tasks: 30. PRs: 8 (cascade).**

---

## M-c1.0 — LLM entitlement (`llm.mode = off`)

The roadmap text says *"`entitlements.llm = none` ← enforced by B0"*, but `Entitlements` today has no `llm` field. We add one. The supervisor reads it; LLM client construction returns an error if `mode == Off`.

### Task M-c1.0.1: Add `LlmEntitlement` schema

**Files:** Create `mur-common/src/bridge/{mod.rs,llm_entitlement.rs}`. Modify `mur-common/src/{lib.rs,agent.rs}`.

- [x] **Step 1: Failing test** — append to a new `mur-common/src/bridge/llm_entitlement.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_mode_is_allowed() {
        let e: LlmEntitlement = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(e.mode, LlmMode::Allowed);
    }
    #[test]
    fn mode_off_round_trips() {
        let e = LlmEntitlement { mode: LlmMode::Off };
        let s = serde_yaml_ng::to_string(&e).unwrap();
        assert!(s.contains("mode: off"));
        assert_eq!(serde_yaml_ng::from_str::<LlmEntitlement>(&s).unwrap(), e);
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common llm_entitlement`. Expect: module not found.

- [x] **Step 3: Implement** — write the module body above the test:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmMode {
    Allowed,
    Off,
}
impl Default for LlmMode { fn default() -> Self { LlmMode::Allowed } }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LlmEntitlement {
    #[serde(default)]
    pub mode: LlmMode,
}
```

Create `mur-common/src/bridge/mod.rs`:

```rust
pub mod llm_entitlement;
pub use llm_entitlement::{LlmEntitlement, LlmMode};
```

In `mur-common/src/lib.rs`, add `pub mod bridge;` and `pub use bridge::{LlmEntitlement, LlmMode};` near the other `pub use` re-exports.

In `mur-common/src/agent.rs`, add to `Entitlements` struct (last field):

```rust
    /// LLM call permission. Default = Allowed (back-compat). Bridges set to Off
    /// so the supervisor refuses to construct an LLM client.
    #[serde(default)]
    pub llm: crate::bridge::llm_entitlement::LlmEntitlement,
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common llm_entitlement` (2 tests) and `cargo test -p mur-common --lib agent::tests` (existing AgentProfile tests still parse legacy YAML).

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/ mur-common/src/agent.rs mur-common/src/lib.rs && git commit -m "M-c1.0.1: LlmEntitlement { mode: allowed|off } + AgentProfile field"`

### Task M-c1.0.2: Supervisor refuses LLM when `mode = Off`

**Files:** Locate LLM builder in `mur-agent-runtime/src/llm/`; create `mur-agent-runtime/tests/bridge_llm_off_blocks.rs`.

- [x] **Step 1: Locate LLM builder** — `grep -rn "fn build\|fn new" mur-agent-runtime/src/llm/ | head`. Find the public entry, e.g. `pub fn build_client(profile: &AgentProfile) -> Result<…>`.

- [x] **Step 2: Failing test** — `mur-agent-runtime/tests/bridge_llm_off_blocks.rs`:

```rust
use mur_common::{AgentProfile, LlmMode};

fn profile_with_llm_off() -> AgentProfile {
    let yaml = include_str!("fixtures/minimal_profile.yaml");
    let mut p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    p.entitlements.llm.mode = LlmMode::Off;
    p
}

#[test]
fn llm_off_blocks_construction() {
    let profile = profile_with_llm_off();
    let err = mur_agent_runtime::llm::build_client(&profile)
        .expect_err("llm.mode=off must block");
    assert!(err.to_string().contains("llm.mode = off"));
}
```

If `mur-agent-runtime/tests/fixtures/minimal_profile.yaml` doesn't exist, build it from the working YAML around `mur-common/src/agent.rs:702` (the round-trip test fixture). The exact byte sequence is too long to inline here; copy the YAML inline string from that test file.

- [x] **Step 3: Verify FAIL** — `cargo test -p mur-agent-runtime --test bridge_llm_off_blocks`.

- [x] **Step 4: Add guard** — at top of the LLM builder identified in Step 1:

```rust
use mur_common::LlmMode;

pub fn build_client(profile: &AgentProfile) -> anyhow::Result<…> {
    if profile.entitlements.llm.mode == LlmMode::Off {
        anyhow::bail!(
            "llm.mode = off — agent '{}' is a bridge and may not call an LLM",
            profile.name
        );
    }
    // … existing body …
}
```

(Adjust signature to match the actual function — the guard is the only new code.)

- [x] **Step 5: Verify PASS** — `cargo test -p mur-agent-runtime --test bridge_llm_off_blocks`.

- [x] **Step 6: Commit** — `git add mur-agent-runtime/src/llm/ mur-agent-runtime/tests/bridge_llm_off_blocks.rs mur-agent-runtime/tests/fixtures/ && git commit -m "M-c1.0.2: supervisor refuses LLM when llm.mode = off"`

### Task M-c1.0.3: One-line CLAUDE.md doc

- [x] **Step 1: Edit** — In `CLAUDE.md`, in the "Agent Runtime (murmur P0a)" section, append:

> `entitlements.llm.mode` is `allowed` by default; bridges set it to `off` so the supervisor refuses to construct an LLM client. See `docs/cookbook/c1-a2a-bridge.md`.

- [x] **Step 2: Commit** — `git add CLAUDE.md && git commit -m "M-c1.0.3: doc llm entitlement"`

---

## M-c1.1 — `routes.yaml` schema + precedence

### Task M-c1.1.1: Schema types + serde round-trip

**Files:** Create `mur-common/src/bridge/routes.rs`.

- [x] **Step 1: Failing test** — write the test alongside types in `routes.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = r#"
default_route: coach
routes:
  - match: { platform: telegram, mention: "@coach" }
    agent: coach
  - match: { platform: telegram, chat_id: "12345" }
    agent: therapist
  - match: { platform: telegram, chat_id: "67890" }
    agent: coach
    fanout: [coach, journal_agent]
"#;
    #[test]
    fn parses_full_example() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.default_route, "coach");
        assert_eq!(cfg.routes.len(), 3);
    }
    #[test]
    fn round_trip_preserves_fields() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let s = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<BridgeRouteConfig>(&s).unwrap(), cfg);
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common bridge::routes`.

- [x] **Step 3: Implement** — top of `routes.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeRouteConfig {
    pub default_route: String,
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteEntry {
    #[serde(rename = "match")]
    pub match_: RouteMatch,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fanout: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteMatch {
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
}
```

In `mur-common/src/bridge/mod.rs`, append `pub mod routes; pub use routes::{BridgeRouteConfig, RouteEntry, RouteMatch};`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common bridge::routes`.

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/routes.rs mur-common/src/bridge/mod.rs && git commit -m "M-c1.1.1: BridgeRouteConfig schema + serde round-trip"`

### Task M-c1.1.2: `InboundMessage` + `resolve` (default fallback)

**Files:** Modify `mur-common/src/bridge/routes.rs`.

- [x] **Step 1: Failing test** — append:

```rust
#[test]
fn resolve_falls_back_to_default() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
    let r = cfg.resolve(&InboundMessage {
        platform: "telegram".into(),
        chat_id: "99999".into(),
        body: "hello".into(),
    });
    assert_eq!(r.recipients(), vec!["coach"]);
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common bridge::routes::tests::resolve_falls_back`.

- [x] **Step 3: Implement** — append to `routes.rs`:

```rust
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub platform: String,
    pub chat_id: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    primary: String,
    fanout: Vec<String>,
}

impl Resolution {
    pub fn recipients(&self) -> Vec<String> {
        if self.fanout.is_empty() { vec![self.primary.clone()] } else { self.fanout.clone() }
    }
}

impl BridgeRouteConfig {
    pub fn resolve(&self, inbound: &InboundMessage) -> Resolution {
        // Pass 1: mention (highest priority)
        for entry in &self.routes {
            if entry.match_.platform != inbound.platform { continue; }
            if let Some(m) = &entry.match_.mention
                && inbound.body.contains(m.as_str())
            {
                return Resolution { primary: entry.agent.clone(), fanout: entry.fanout.clone() };
            }
        }
        // Pass 2: platform + chat_id (skip mention-routes; they only match in pass 1)
        for entry in &self.routes {
            if entry.match_.platform != inbound.platform { continue; }
            if entry.match_.mention.is_some() { continue; }
            if let Some(c) = &entry.match_.chat_id
                && c == &inbound.chat_id
            {
                return Resolution { primary: entry.agent.clone(), fanout: entry.fanout.clone() };
            }
        }
        // Pass 3: default
        Resolution { primary: self.default_route.clone(), fanout: vec![] }
    }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common bridge::routes`.

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/routes.rs && git commit -m "M-c1.1.2: BridgeRouteConfig::resolve fallback"`

### Task M-c1.1.3: Precedence — mention > chat_id > default + fanout

**Files:** Modify `mur-common/src/bridge/routes.rs` tests only.

- [x] **Step 1: Add tests** — append to `mod tests`:

```rust
#[test]
fn mention_wins_over_chat_id() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
    let r = cfg.resolve(&InboundMessage {
        platform: "telegram".into(),
        chat_id: "12345".into(), // would route to therapist
        body: "hey @coach help".into(), // mention wins
    });
    assert_eq!(r.recipients(), vec!["coach"]);
}

#[test]
fn chat_id_when_no_mention() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
    let r = cfg.resolve(&InboundMessage {
        platform: "telegram".into(),
        chat_id: "12345".into(),
        body: "no mentions".into(),
    });
    assert_eq!(r.recipients(), vec!["therapist"]);
}

#[test]
fn fanout_returns_full_list() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
    let r = cfg.resolve(&InboundMessage {
        platform: "telegram".into(),
        chat_id: "67890".into(),
        body: "ping".into(),
    });
    assert_eq!(r.recipients(), vec!["coach", "journal_agent"]);
}

#[test]
fn platform_mismatch_falls_through() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
    let r = cfg.resolve(&InboundMessage {
        platform: "slack".into(),
        chat_id: "12345".into(),
        body: "ping".into(),
    });
    assert_eq!(r.recipients(), vec!["coach"]);
}
```

- [x] **Step 2: Verify PASS** — `cargo test -p mur-common bridge::routes`. The resolver from M-c1.1.2 already implements this; if any test fails, fix the resolver until green.

- [x] **Step 3: Commit** — `git add mur-common/src/bridge/routes.rs && git commit -m "M-c1.1.3: precedence + fanout tests"`

### Task M-c1.1.4: Structural validator

- [x] **Step 1: Failing tests** — append to `mod tests`:

```rust
#[test]
fn empty_default_route_rejected() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str("default_route: \"\"\nroutes: []\n").unwrap();
    assert!(cfg.validate().is_err());
}
#[test]
fn nonempty_validates() {
    let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
    assert!(cfg.validate().is_ok());
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common bridge::routes::tests::empty_default_route`.

- [x] **Step 3: Implement** — append to the `impl BridgeRouteConfig` block:

```rust
pub fn validate(&self) -> Result<(), String> {
    if self.default_route.trim().is_empty() {
        return Err("default_route must be non-empty".into());
    }
    for (i, e) in self.routes.iter().enumerate() {
        if e.agent.trim().is_empty() { return Err(format!("routes[{i}].agent empty")); }
        if e.match_.platform.trim().is_empty() {
            return Err(format!("routes[{i}].match.platform empty"));
        }
    }
    Ok(())
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common bridge::routes`.

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/routes.rs && git commit -m "M-c1.1.4: BridgeRouteConfig::validate"`

---

## M-c1.2 — sled-backed `DedupeStore`

### Task M-c1.2.1: Add sled dep + skeleton

- [x] **Step 1: Workspace dep** — in root `Cargo.toml` `[workspace.dependencies]`, append `sled = "0.34"`. In `mur-agent-runtime/Cargo.toml` `[dependencies]`, append `sled = { workspace = true }`.

- [x] **Step 2: Skeleton** — create `mur-agent-runtime/src/bridge/mod.rs`:

```rust
//! # Bridge support for the A2A runtime
//!
//! A "bridge" is a small, LLM-less mur agent that ferries messages between
//! a chat platform and a user agent. Envelope verification runs **regardless
//! of transport** (Unix socket has no peer auth; Noise XK only proves *some*
//! peer's identity, not authorization to claim the bridge role).

pub mod dedupe;
```

Create `mur-agent-runtime/src/bridge/dedupe.rs`:

```rust
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const TREE_NAME: &str = "seen";
/// Sweep TTL-expired keys every Nth lookup. 256 = busy bridges sweep ~once
/// per few hundred messages; idle bridges almost never.
const SWEEP_EVERY: u32 = 256;

#[derive(thiserror::Error, Debug)]
pub enum DedupeError {
    #[error("sled error: {0}")]
    Sled(#[from] sled::Error),
    #[error("system time: {0}")]
    Time(#[from] std::time::SystemTimeError),
}

pub struct DedupeStore {
    _db: sled::Db,
    tree: sled::Tree,
    bridge_id: String,
    counter: std::sync::atomic::AtomicU32,
}

impl DedupeStore {
    pub fn open(dir: &Path, bridge_id: impl Into<String>) -> Result<Self, DedupeError> {
        let db = sled::open(dir.join("seen.sled"))?;
        let tree = db.open_tree(TREE_NAME)?;
        Ok(Self { _db: db, tree, bridge_id: bridge_id.into(), counter: 0.into() })
    }
}
```

In `mur-agent-runtime/src/lib.rs`, add `pub mod bridge;`.

- [x] **Step 3: Build** — `cargo build -p mur-agent-runtime`. Warnings about unused fields are fine.

- [x] **Step 4: Commit** — `git add Cargo.toml Cargo.lock mur-agent-runtime/Cargo.toml mur-agent-runtime/src/bridge/ mur-agent-runtime/src/lib.rs && git commit -m "M-c1.2.1: sled dep + DedupeStore::open skeleton"`

### Task M-c1.2.2: `mark_seen` / `is_seen`

- [x] **Step 1: Failing test** — `mur-agent-runtime/tests/bridge_dedupe_sled.rs`:

```rust
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use tempfile::TempDir;

#[test]
fn mark_then_is_seen_returns_true() {
    let tmp = TempDir::new().unwrap();
    let mut s = DedupeStore::open(tmp.path(), "bridge_telegram").unwrap();
    assert!(!s.is_seen("msg-42").unwrap());
    s.mark_seen("msg-42").unwrap();
    assert!(s.is_seen("msg-42").unwrap());
}

#[test]
fn unseen_returns_false() {
    let tmp = TempDir::new().unwrap();
    let s = DedupeStore::open(tmp.path(), "x").unwrap();
    assert!(!s.is_seen("never-marked").unwrap());
}

#[test]
fn different_bridges_independent() {
    let tmp = TempDir::new().unwrap();
    let mut a = DedupeStore::open(tmp.path(), "a").unwrap();
    a.mark_seen("msg-1").unwrap();
    drop(a);
    // separate bridge_id namespace inside the same DB
    let b_dir = tmp.path().join("b");
    std::fs::create_dir(&b_dir).unwrap();
    let b = DedupeStore::open(&b_dir, "b").unwrap();
    assert!(!b.is_seen("msg-1").unwrap());
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test bridge_dedupe_sled`.

- [x] **Step 3: Implement** — append to `dedupe.rs`:

```rust
impl DedupeStore {
    fn make_key(&self, msg_id: &str) -> Vec<u8> {
        let mut k = Vec::with_capacity(self.bridge_id.len() + 1 + msg_id.len());
        k.extend_from_slice(self.bridge_id.as_bytes());
        k.push(0);
        k.extend_from_slice(msg_id.as_bytes());
        k
    }

    pub fn mark_seen(&mut self, msg_id: &str) -> Result<(), DedupeError> {
        let key = self.make_key(msg_id);
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        self.tree.insert(&key, &now.to_le_bytes())?;
        Ok(())
    }

    pub fn is_seen(&self, msg_id: &str) -> Result<bool, DedupeError> {
        let key = self.make_key(msg_id);
        let hit = self.tree.get(&key)?.is_some();
        let n = self.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n.wrapping_add(1) % SWEEP_EVERY == 0 {
            let _ = self.sweep_expired();
        }
        Ok(hit)
    }

    pub(crate) fn sweep_expired(&self) -> Result<usize, DedupeError> {
        Ok(0) // implemented in M-c1.2.3
    }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test bridge_dedupe_sled` (3 tests).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/src/bridge/dedupe.rs mur-agent-runtime/tests/bridge_dedupe_sled.rs && git commit -m "M-c1.2.2: mark_seen / is_seen w/ bridge_id namespace"`

### Task M-c1.2.3: TTL eviction

- [x] **Step 1: Failing test** — append to `bridge_dedupe_sled.rs`:

```rust
#[test]
fn ttl_eviction_removes_old_entries() {
    let tmp = TempDir::new().unwrap();
    let mut s = DedupeStore::open(tmp.path(), "bridge").unwrap();
    let stale_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - (8 * 24 * 60 * 60);
    s.insert_at_for_test("stale", stale_ts).unwrap();
    s.mark_seen("fresh").unwrap();
    let evicted = s.sweep_expired().unwrap();
    assert_eq!(evicted, 1);
    assert!(!s.is_seen("stale").unwrap());
    assert!(s.is_seen("fresh").unwrap());
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test bridge_dedupe_sled ttl_eviction`.

- [x] **Step 3: Replace stub + add helper** — replace `sweep_expired` body:

```rust
pub fn sweep_expired(&self) -> Result<usize, DedupeError> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let cutoff = now.saturating_sub(TTL.as_secs());
    let mut evicted = 0;
    for kv in self.tree.iter() {
        let (key, value) = kv?;
        if value.len() != 8 { continue; }
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&value);
        if u64::from_le_bytes(ts) < cutoff {
            self.tree.remove(&key)?;
            evicted += 1;
        }
    }
    Ok(evicted)
}

#[doc(hidden)]
pub fn insert_at_for_test(&mut self, msg_id: &str, ts_secs: u64) -> Result<(), DedupeError> {
    let key = self.make_key(msg_id);
    self.tree.insert(&key, &ts_secs.to_le_bytes())?;
    Ok(())
}
```

(Remove the now-redundant `pub(crate)` `sweep_expired` stub above — replace it in place.)

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test bridge_dedupe_sled` (4 tests).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/src/bridge/dedupe.rs mur-agent-runtime/tests/bridge_dedupe_sled.rs && git commit -m "M-c1.2.3: TTL sweep evicts entries > 7d"`

### Task M-c1.2.4: Persistence + idempotency

- [x] **Step 1: Tests** — append:

```rust
#[test]
fn reopen_preserves_state() {
    let tmp = TempDir::new().unwrap();
    {
        let mut s = DedupeStore::open(tmp.path(), "x").unwrap();
        s.mark_seen("durable").unwrap();
    }
    let s = DedupeStore::open(tmp.path(), "x").unwrap();
    assert!(s.is_seen("durable").unwrap());
}

#[test]
fn double_mark_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let mut s = DedupeStore::open(tmp.path(), "x").unwrap();
    s.mark_seen("dup").unwrap();
    s.mark_seen("dup").unwrap();
    assert!(s.is_seen("dup").unwrap());
}
```

- [x] **Step 2: Verify PASS** — `cargo test -p mur-agent-runtime --test bridge_dedupe_sled` (6 tests). Sled flushes on Drop in v0.34, so persistence works without explicit flush.

- [x] **Step 3: Commit** — `git add mur-agent-runtime/tests/bridge_dedupe_sled.rs && git commit -m "M-c1.2.4: persistence + idempotency"`

---

## M-c1.3 — `SignedEnvelope` + `TrustedPeer` + verify

### Task M-c1.3.1: `SignedEnvelope` struct

**Files:** Create `mur-common/src/bridge/envelope.rs`. Verify `serde_bytes` is in `mur-common/Cargo.toml`; add `serde_bytes = "0.11"` if missing (`grep -n serde_bytes mur-common/Cargo.toml`).

- [x] **Step 1: Failing test** — `mur-common/src/bridge/envelope.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_payload_is_passthrough() {
        let payload = serde_json::json!({"a": 1}).to_string().into_bytes();
        let e = SignedEnvelope {
            payload: payload.clone(),
            sig: vec![0u8; 64],
            key_version: 1,
            bridge_pubkey_multibase: "z".into(),
        };
        assert_eq!(e.canonical_payload_for_signing(), payload.as_slice());
    }
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common bridge::envelope`.

- [x] **Step 3: Implement** — top of `envelope.rs`:

```rust
use serde::{Deserialize, Serialize};

/// Bridge-signed wrapper around an A2A payload. `payload` is the *already-
/// canonical* JSON-serialized A2A `JsonRpcRequest`; the bridge canonicalizes
/// (sorted keys, no whitespace) BEFORE construction. Verification re-uses
/// these exact bytes — never re-canonicalize on receive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEnvelope {
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub sig: Vec<u8>,
    pub key_version: u32,
    pub bridge_pubkey_multibase: String,
}

impl SignedEnvelope {
    pub fn canonical_payload_for_signing(&self) -> &[u8] { &self.payload }
}
```

In `mur-common/src/bridge/mod.rs`, append `pub mod envelope; pub use envelope::SignedEnvelope;`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common bridge::envelope`.

- [x] **Step 5: Commit** — `git add mur-common/Cargo.toml mur-common/src/bridge/ && git commit -m "M-c1.3.1: SignedEnvelope schema"`

### Task M-c1.3.2: `sign_payload` / `verify_envelope_with_pubkey`

- [x] **Step 1: Failing tests** — append to `envelope.rs` `mod tests`:

```rust
#[test]
fn sign_then_verify_round_trips() {
    use crate::identity::AgentIdentity;
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hello".to_vec(), &id, 7);
    assert_eq!(env.key_version, 7);
    verify_envelope_with_pubkey(&env, &env.bridge_pubkey_multibase).unwrap();
}

#[test]
fn verify_with_wrong_pubkey_fails() {
    use crate::identity::{AgentIdentity, encode_pubkey};
    let a = AgentIdentity::generate();
    let b = AgentIdentity::generate();
    let env = sign_payload(b"x".to_vec(), &a, 0);
    let pub_b = encode_pubkey(&b.verifying_key());
    assert!(matches!(
        verify_envelope_with_pubkey(&env, &pub_b).unwrap_err(),
        EnvelopeError::SignatureMismatch
    ));
}

#[test]
fn tampered_payload_fails() {
    use crate::identity::AgentIdentity;
    let id = AgentIdentity::generate();
    let mut env = sign_payload(b"orig".to_vec(), &id, 0);
    env.payload = b"tamper".to_vec();
    let pub_ = env.bridge_pubkey_multibase.clone();
    assert!(matches!(
        verify_envelope_with_pubkey(&env, &pub_).unwrap_err(),
        EnvelopeError::SignatureMismatch
    ));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-common bridge::envelope`.

- [x] **Step 3: Implement** — append to `envelope.rs`:

```rust
use ed25519_dalek::Signer;

#[derive(thiserror::Error, Debug)]
pub enum EnvelopeError {
    #[error("multibase decode: {0}")]
    Multibase(#[from] crate::identity::IdentityError),
    #[error("bad sig length: expected 64, got {0}")]
    BadSigLen(usize),
    #[error("signature does not verify")]
    SignatureMismatch,
    #[error("untrusted peer")]
    UntrustedPeer,
}

pub fn sign_payload(
    payload: Vec<u8>,
    identity: &crate::identity::AgentIdentity,
    key_version: u32,
) -> SignedEnvelope {
    let sig = identity.signing_key().sign(&payload);
    SignedEnvelope {
        payload,
        sig: sig.to_bytes().to_vec(),
        key_version,
        bridge_pubkey_multibase: crate::identity::encode_pubkey(&identity.verifying_key()),
    }
}

pub fn verify_envelope_with_pubkey(
    env: &SignedEnvelope,
    expected_pubkey: &str,
) -> Result<(), EnvelopeError> {
    use ed25519_dalek::{Signature, VerifyingKey};
    if env.sig.len() != 64 { return Err(EnvelopeError::BadSigLen(env.sig.len())); }
    let pub_bytes = crate::identity::decode_pubkey(expected_pubkey)?;
    let vk = VerifyingKey::from_bytes(&pub_bytes)
        .map_err(|_| EnvelopeError::SignatureMismatch)?;
    let sig_arr: [u8; 64] = env.sig.as_slice().try_into().unwrap();
    let sig = Signature::from_bytes(&sig_arr);
    vk.verify_strict(&env.payload, &sig).map_err(|_| EnvelopeError::SignatureMismatch)?;
    Ok(())
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-common bridge::envelope` (4 tests).

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/envelope.rs && git commit -m "M-c1.3.2: sign_payload + verify_envelope_with_pubkey"`

### Task M-c1.3.3: `TrustedPeer` + `AgentProfile.trusted_peers`

- [x] **Step 1: Failing test** — `mur-common/src/bridge/peer.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub pubkey_multibase: String,
    pub name: String,
    /// Optional version pin. None = any key_version with matching pubkey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_version: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let p = TrustedPeer {
            pubkey_multibase: "z6Mk".into(),
            name: "tg_bridge".into(),
            key_version: Some(3),
        };
        let s = serde_yaml_ng::to_string(&p).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<TrustedPeer>(&s).unwrap(), p);
    }
}
```

In `mur-common/src/bridge/mod.rs`, append `pub mod peer; pub use peer::TrustedPeer;`.

- [x] **Step 2: Verify** — `cargo test -p mur-common bridge::peer`.

- [x] **Step 3: Add to AgentProfile** — in `mur-common/src/agent.rs` `AgentProfile`, before `created_at`:

```rust
    /// Pubkeys of bridges (and other LLM-less peers) this agent will accept
    /// signed envelopes from. Empty = accept no bridge traffic. Default = empty.
    #[serde(default)]
    pub trusted_peers: Vec<crate::bridge::peer::TrustedPeer>,
```

- [x] **Step 4: Verify back-compat** — `cargo test -p mur-common --lib agent::tests`.

- [x] **Step 5: Commit** — `git add mur-common/src/bridge/peer.rs mur-common/src/bridge/mod.rs mur-common/src/agent.rs && git commit -m "M-c1.3.3: TrustedPeer + AgentProfile.trusted_peers"`

### Task M-c1.3.4: `verify_inbound_envelope`

- [x] **Step 1: Failing test** — `mur-agent-runtime/tests/bridge_envelope_signing.rs`:

```rust
use mur_agent_runtime::bridge::verify::verify_inbound_envelope;
use mur_common::bridge::envelope::{sign_payload, EnvelopeError};
use mur_common::bridge::peer::TrustedPeer;
use mur_common::identity::{encode_pubkey, AgentIdentity};

fn peer_for(id: &AgentIdentity, ver: Option<u32>) -> TrustedPeer {
    TrustedPeer {
        pubkey_multibase: encode_pubkey(&id.verifying_key()),
        name: "stub".into(),
        key_version: ver,
    }
}

#[test]
fn signed_by_trusted_passes() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 0);
    verify_inbound_envelope(&env, &[peer_for(&id, None)]).unwrap();
}

#[test]
fn unknown_peer_rejected() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 0);
    assert!(matches!(
        verify_inbound_envelope(&env, &[]).unwrap_err(),
        EnvelopeError::UntrustedPeer
    ));
}

#[test]
fn key_version_pin_mismatch_rejected() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 1);
    assert!(matches!(
        verify_inbound_envelope(&env, &[peer_for(&id, Some(2))]).unwrap_err(),
        EnvelopeError::UntrustedPeer
    ));
}

#[test]
fn key_version_pin_match_accepted() {
    let id = AgentIdentity::generate();
    let env = sign_payload(b"hi".to_vec(), &id, 4);
    verify_inbound_envelope(&env, &[peer_for(&id, Some(4))]).unwrap();
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test bridge_envelope_signing`.

- [x] **Step 3: Implement** — `mur-agent-runtime/src/bridge/verify.rs`:

```rust
use mur_common::bridge::envelope::{verify_envelope_with_pubkey, EnvelopeError, SignedEnvelope};
use mur_common::bridge::peer::TrustedPeer;

/// Verify a `SignedEnvelope` against an explicit trust list. Returns Ok(()) only if:
///   1. envelope's `bridge_pubkey_multibase` matches some `TrustedPeer`
///   2. that peer's pinned `key_version`, if any, matches the envelope
///   3. the Ed25519 signature verifies against `payload`
pub fn verify_inbound_envelope(
    env: &SignedEnvelope,
    peers: &[TrustedPeer],
) -> Result<(), EnvelopeError> {
    let peer = peers
        .iter()
        .find(|p| p.pubkey_multibase == env.bridge_pubkey_multibase)
        .ok_or(EnvelopeError::UntrustedPeer)?;
    if let Some(pinned) = peer.key_version
        && pinned != env.key_version
    {
        return Err(EnvelopeError::UntrustedPeer);
    }
    verify_envelope_with_pubkey(env, &peer.pubkey_multibase)
}
```

In `mur-agent-runtime/src/bridge/mod.rs`, append `pub mod verify;`.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test bridge_envelope_signing` (4 tests).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/src/bridge/verify.rs mur-agent-runtime/src/bridge/mod.rs mur-agent-runtime/tests/bridge_envelope_signing.rs && git commit -m "M-c1.3.4: verify_inbound_envelope rejects untrusted + version-mismatched"`

### Task M-c1.3.5: Module-level transport-independence doc

- [x] **Step 1: Edit** — In `mur-agent-runtime/src/bridge/mod.rs`, the module doc was added in M-c1.2.1 — append a "Trust model" section to the existing `//!` block:

```rust
//! ## Trust model
//!
//! Verification ([`verify::verify_inbound_envelope`]) runs **regardless of
//! transport**. Every inbound A2A envelope from a bridge MUST carry a
//! `SignedEnvelope` and the user agent MUST verify against
//! `profile.yaml.trusted_peers[]` before processing.
//!
//! ## Sub-modules
//!
//! - [`dedupe`] — sled-backed `(bridge_id, platform_msg_id)`, 7-day TTL
//! - [`verify`] — envelope verification against a trust list
```

- [x] **Step 2: Build** — `cargo build -p mur-agent-runtime`.

- [x] **Step 3: Commit** — `git add mur-agent-runtime/src/bridge/mod.rs && git commit -m "M-c1.3.5: doc transport-independence of envelope verify"`

---

## M-c1.4 — Heartbeat + degraded surface

### Task M-c1.4.1: `BridgeBeacon` 30 s emit loop

- [x] **Step 1: Failing test** — `mur-agent-runtime/src/bridge/beacon.rs`:

```rust
use std::time::Duration;
use tokio::sync::mpsc::Sender;

pub const METHOD_BRIDGE_ALIVE: &str = "telemetry/bridge_alive";

pub fn make_alive_payload(bridge_id: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": METHOD_BRIDGE_ALIVE,
        "params": { "bridge_id": bridge_id, "ts": chrono::Utc::now().to_rfc3339() }
    })).expect("static JSON serializes")
}

pub struct BridgeBeacon { bridge_id: String, tx: Sender<Vec<u8>>, interval: Duration }

impl BridgeBeacon {
    pub fn new(bridge_id: impl Into<String>, tx: Sender<Vec<u8>>) -> Self {
        Self { bridge_id: bridge_id.into(), tx, interval: Duration::from_secs(30) }
    }
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut t = tokio::time::interval(self.interval);
            loop {
                t.tick().await;
                if self.tx.send(make_alive_payload(&self.bridge_id)).await.is_err() {
                    break;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_has_method_and_bridge_id() {
        let p = make_alive_payload("bridge_telegram");
        let v: serde_json::Value = serde_json::from_slice(&p).unwrap();
        assert_eq!(v["method"], "telemetry/bridge_alive");
        assert_eq!(v["params"]["bridge_id"], "bridge_telegram");
    }
}
```

In `mur-agent-runtime/src/bridge/mod.rs`, append `pub mod beacon;`.

- [x] **Step 2: Verify** — `cargo test -p mur-agent-runtime bridge::beacon::tests`. PASS.

- [x] **Step 3: Commit** — `git add mur-agent-runtime/src/bridge/beacon.rs mur-agent-runtime/src/bridge/mod.rs && git commit -m "M-c1.4.1: BridgeBeacon emits telemetry/bridge_alive every 30 s"`

### Task M-c1.4.2: `bridge_status_for_peer` (degraded threshold)

- [x] **Step 1: Failing test** — `mur-agent-runtime/tests/bridge_beacon_degraded.rs`:

```rust
use mur_agent_runtime::bridge::beacon::{bridge_status_for_peer, BridgePeerStatus};
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn write_lock_with_age(dir: &std::path::Path, age: Duration) {
    let lock = dir.join("running.lock");
    std::fs::write(&lock, b"{}").unwrap();
    let f = std::fs::File::open(&lock).unwrap();
    f.set_modified(SystemTime::now() - age).unwrap();
}

#[test]
fn fresh_lock_is_running() {
    let tmp = TempDir::new().unwrap();
    write_lock_with_age(tmp.path(), Duration::from_secs(5));
    assert_eq!(bridge_status_for_peer(tmp.path()), BridgePeerStatus::Running);
}
#[test]
fn old_lock_is_degraded() {
    let tmp = TempDir::new().unwrap();
    write_lock_with_age(tmp.path(), Duration::from_secs(120));
    assert_eq!(bridge_status_for_peer(tmp.path()), BridgePeerStatus::Degraded);
}
#[test]
fn missing_lock_is_offline() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(bridge_status_for_peer(tmp.path()), BridgePeerStatus::Offline);
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-agent-runtime --test bridge_beacon_degraded`.

- [x] **Step 3: Implement** — append to `beacon.rs`:

```rust
use std::path::Path;
use std::time::SystemTime;

pub const DEGRADED_AFTER_SECS: u64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgePeerStatus { Running, Degraded, Offline }

/// Classify a bridge peer by inspecting `running.lock` mtime in `agent_dir`.
pub fn bridge_status_for_peer(agent_dir: &Path) -> BridgePeerStatus {
    let meta = match std::fs::metadata(agent_dir.join("running.lock")) {
        Ok(m) => m,
        Err(_) => return BridgePeerStatus::Offline,
    };
    let mtime = match meta.modified() {
        Ok(t) => t,
        Err(_) => return BridgePeerStatus::Offline,
    };
    let age = SystemTime::now().duration_since(mtime).unwrap_or_default().as_secs();
    if age > DEGRADED_AFTER_SECS { BridgePeerStatus::Degraded } else { BridgePeerStatus::Running }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-agent-runtime --test bridge_beacon_degraded` (3 tests).

- [x] **Step 5: Commit** — `git add mur-agent-runtime/src/bridge/beacon.rs mur-agent-runtime/tests/bridge_beacon_degraded.rs && git commit -m "M-c1.4.2: bridge_status_for_peer (running/degraded/offline)"`

### Task M-c1.4.3: Supervisor spawns beacon when `llm.mode = off`

- [x] **Step 1: Locate supervisor** — `grep -n "Acquire running.lock\|Write running.lock\|telemetry_tx" mur-agent-runtime/src/supervisor.rs | head`. Land near `// 8. Write running.lock` (line ~438).

- [x] **Step 2: Insert spawn** — after `running.lock` is written and `telemetry_tx` is in scope:

```rust
// 8.5 — bridge agents emit a 30 s heartbeat beacon
if profile.entitlements.llm.mode == mur_common::LlmMode::Off {
    let beacon = crate::bridge::beacon::BridgeBeacon::new(
        profile.name.clone(),
        telemetry_tx.clone(),
    );
    background_tasks.push(beacon.spawn());
    tracing::info!(name = %profile.name, "spawned BridgeBeacon (30 s heartbeat)");
}
```

If `background_tasks: Vec<JoinHandle<()>>` doesn't already exist in `run()`, declare it near other locals and abort each on shutdown:

```rust
let mut background_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
// … shutdown:
for h in background_tasks { h.abort(); }
```

- [x] **Step 3: Build** — `cargo build -p mur-agent-runtime`. Resolve any `Sender<Vec<u8>>` mismatch by checking the actual type of the runtime's telemetry channel; if it's `Sender<TelemetryEvent>` instead, wrap `make_alive_payload(...)` in whatever wrapper the channel expects (look at how existing `METHOD_HEARTBEAT` usage at `mur-agent-runtime/src/telemetry_writer.rs:189` builds events, and mirror that pattern in `BridgeBeacon::spawn`).

- [x] **Step 4: Commit** — `git add mur-agent-runtime/src/supervisor.rs mur-agent-runtime/src/bridge/beacon.rs && git commit -m "M-c1.4.3: supervisor spawns BridgeBeacon when llm.mode = off"`

### Task M-c1.4.4: `mur agent doctor` surfaces `bridges:` section

Note: M-c1.6.2 will introduce the canonical bridge `profile.yaml` template inside `scaffold_stub_bridge` — for this task we just write a minimal in-line fixture, since this milestone lands first.

- [x] **Step 1: Failing test** — `mur-core/tests/doctor_bridges_section.rs`:

```rust
use mur_core::cmd::doctor::{collect_bridge_statuses, BridgeSummary};
use mur_agent_runtime::bridge::beacon::BridgePeerStatus;
use tempfile::TempDir;

fn write_bridge_fixture(dir: &std::path::Path) {
    // Minimal AgentProfile YAML w/ entitlements.llm.mode = off. Copy/paste
    // from mur-common/src/agent.rs:702 round-trip fixture and inject
    // `entitlements.llm: { mode: off }`. Inline string keeps the test hermetic.
    std::fs::write(dir.join("profile.yaml"),
        include_str!("fixtures/bridge_profile.yaml")).unwrap();
}

#[test]
fn lists_running_and_degraded() {
    let tmp = TempDir::new().unwrap();
    let agents = tmp.path().join("agents");
    let a = agents.join("bridge_a");
    let b = agents.join("bridge_b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    write_bridge_fixture(&a);
    write_bridge_fixture(&b);
    std::fs::write(a.join("running.lock"), b"{}").unwrap();
    let stale = b.join("running.lock");
    std::fs::write(&stale, b"{}").unwrap();
    std::fs::File::open(&stale).unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
        .unwrap();

    let summary = collect_bridge_statuses(tmp.path());
    let map: std::collections::BTreeMap<_,_> = summary.iter().map(|s: &BridgeSummary| (s.name.clone(), s.status)).collect();
    assert_eq!(map["bridge_a"], BridgePeerStatus::Running);
    assert_eq!(map["bridge_b"], BridgePeerStatus::Degraded);
}
```

Create `mur-core/tests/fixtures/bridge_profile.yaml` by copying the round-trip YAML at `mur-common/src/agent.rs:702`+, replacing the `name:`/`display_name:` with a generic value, and injecting `entitlements.llm: { mode: off }` into the existing `entitlements:` block.

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test doctor_bridges_section`.

- [x] **Step 3: Implement** — append to `mur-core/src/cmd/doctor.rs` (and ensure `pub` so the integration test can import):

```rust
use mur_agent_runtime::bridge::beacon::{bridge_status_for_peer, BridgePeerStatus};

#[derive(Debug)]
pub struct BridgeSummary {
    pub name: String,
    pub status: BridgePeerStatus,
}

pub fn collect_bridge_statuses(mur_home: &std::path::Path) -> Vec<BridgeSummary> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(mur_home.join("agents")) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() { continue; }
        let yaml = match std::fs::read_to_string(dir.join("profile.yaml")) {
            Ok(s) => s, Err(_) => continue,
        };
        let profile: mur_common::AgentProfile = match serde_yaml_ng::from_str(&yaml) {
            Ok(p) => p, Err(_) => continue,
        };
        if profile.entitlements.llm.mode != mur_common::LlmMode::Off { continue; }
        out.push(BridgeSummary { name: profile.name.clone(), status: bridge_status_for_peer(&dir) });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}
```

In the existing `pub fn run(format: &str, json: bool)`, after the existing format-aware output, append:

```rust
if matches!(format, "all" | "") {
    let mur_home = std::env::var("MUR_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().expect("home").join(".mur"));
    let bridges = collect_bridge_statuses(&mur_home);
    if !bridges.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "bridges": bridges.iter().map(|b| serde_json::json!({
                    "name": b.name,
                    "status": match b.status {
                        BridgePeerStatus::Running => "running",
                        BridgePeerStatus::Degraded => "degraded",
                        BridgePeerStatus::Offline => "offline",
                    },
                })).collect::<Vec<_>>(),
            }))?);
        } else {
            println!("\nbridges:");
            for b in &bridges {
                let label = match b.status {
                    BridgePeerStatus::Running => "running",
                    BridgePeerStatus::Degraded => "degraded",
                    BridgePeerStatus::Offline => "offline",
                };
                println!("  {}: {}", b.name, label);
            }
        }
    }
}
```

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test doctor_bridges_section`.

- [x] **Step 5: Commit** — `git add mur-core/src/cmd/doctor.rs mur-core/tests/doctor_bridges_section.rs mur-core/tests/fixtures/ && git commit -m "M-c1.4.4: mur agent doctor surfaces bridges: section"`

---

## M-c1.5 — `AckTracker`

### Task M-c1.5.1: Type + happy path

- [x] **Step 1: Failing test** — `mur-agent-runtime/src/bridge/ack.rs`:

```rust
//! Cursor-advancement helper. **`committed_offset` advances if and only if
//! the user agent returns 2xx.** Generic over `T` so platforms with
//! non-numeric cursors (e.g. Slack `next_cursor: String`) can use it.
//!
//! Concrete bridges MUST persist `committed_offset` to disk before resuming
//! polling so a crash mid-pending does not skip messages.

#[derive(Debug)]
pub struct AckTracker<T: Clone + PartialEq> {
    committed: T,
    pending: Option<T>,
}

impl<T: Clone + PartialEq> AckTracker<T> {
    pub fn new(initial: T) -> Self { Self { committed: initial, pending: None } }
    pub fn committed_offset(&self) -> T { self.committed.clone() }
    /// Idempotent: a second `start_pending` without intervening confirm/reject
    /// replaces the first.
    pub fn start_pending(&mut self, next: T) { self.pending = Some(next); }
    pub fn confirm(&mut self) { if let Some(p) = self.pending.take() { self.committed = p; } }
    pub fn reject(&mut self) { self.pending = None; }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirm_advances() {
        let mut t = AckTracker::new(100);
        t.start_pending(105);
        assert_eq!(t.committed_offset(), 100);
        t.confirm();
        assert_eq!(t.committed_offset(), 105);
    }
    #[test]
    fn reject_keeps_old() {
        let mut t = AckTracker::new(100);
        t.start_pending(105);
        t.reject();
        assert_eq!(t.committed_offset(), 100);
    }
}
```

In `mur-agent-runtime/src/bridge/mod.rs`, append `pub mod ack;`.

- [x] **Step 2: Verify PASS** — `cargo test -p mur-agent-runtime bridge::ack::tests`.

- [x] **Step 3: Commit** — `git add mur-agent-runtime/src/bridge/ack.rs mur-agent-runtime/src/bridge/mod.rs && git commit -m "M-c1.5.1: AckTracker — confirm/reject + invariant doc"`

### Task M-c1.5.2: 5xx-then-2xx integration

- [x] **Step 1: Test** — `mur-agent-runtime/tests/bridge_ack_tracker.rs`:

```rust
use mur_agent_runtime::bridge::ack::AckTracker;

fn simulate(t: &mut AckTracker<u64>, ok: bool, high: u64) {
    t.start_pending(high);
    if ok { t.confirm(); } else { t.reject(); }
}

#[test]
fn five_xx_then_two_xx_recovers() {
    let mut t = AckTracker::new(0u64);
    simulate(&mut t, false, 10);
    assert_eq!(t.committed_offset(), 0);
    simulate(&mut t, true, 10);
    assert_eq!(t.committed_offset(), 10);
    simulate(&mut t, true, 20);
    assert_eq!(t.committed_offset(), 20);
}

#[test]
fn many_failures_pin_offset() {
    let mut t = AckTracker::new(50u64);
    for _ in 0..10 { simulate(&mut t, false, 60); }
    assert_eq!(t.committed_offset(), 50);
}
```

- [x] **Step 2: Verify** — `cargo test -p mur-agent-runtime --test bridge_ack_tracker`. PASS.

- [x] **Step 3: Commit** — `git add mur-agent-runtime/tests/bridge_ack_tracker.rs && git commit -m "M-c1.5.2: AckTracker — 5xx recovery + pinned-on-failures"`

### Task M-c1.5.3: Spec compliance summary in module doc

The invariant doc was already added in M-c1.5.1. This task just verifies it.

- [x] **Step 1: Re-read** — `head -20 mur-agent-runtime/src/bridge/ack.rs`. Confirm the doc lines beginning with `//!` describe (a) "advance iff 2xx" and (b) the persistence requirement.

- [x] **Step 2: Build** — `cargo doc -p mur-agent-runtime --no-deps`. PASS without warnings on the bridge module.

- [x] **Step 3: Commit (if any tweaks)** — `git diff --quiet || git commit -am "M-c1.5.3: confirm AckTracker doc states the invariant"` (no-op if no changes).

---

## M-c1.6 — `mur agent companion connector add --platform stub`

### Task M-c1.6.1: Subcommand wiring

- [x] **Step 1: Locate enum** — `grep -n "enum CompanionCmd" mur-core/src/cmd/agent_companion.rs`.

- [x] **Step 2: Extend enum** — in `agent_companion.rs`, append a variant:

```rust
    /// Manage cross-platform connectors (bridge agents). Track C1+.
    Connector {
        #[command(subcommand)]
        action: ConnectorAction,
    },
```

Below the enum:

```rust
#[derive(clap::Subcommand, Debug)]
pub enum ConnectorAction {
    /// Scaffold a new bridge agent for a given platform.
    Add {
        /// Bridge agent name (distinct from any user agent).
        name: String,
        /// Platform — only "stub" is supported in C1.
        #[arg(long, default_value = "stub")]
        platform: String,
        /// Recipient agent for the default route. Required.
        #[arg(long)]
        default_route: String,
    },
}
```

In the existing `pub async fn run(args: CompanionArgs) -> Result<()>` dispatch match, add an arm:

```rust
    CompanionCmd::Connector { action } => match action {
        ConnectorAction::Add { name, platform, default_route } => {
            crate::cmd::agent_companion::connector::add(name, &platform, &default_route).await?;
        }
    },
```

- [x] **Step 3: Stub** — create `mur-core/src/cmd/agent_companion/connector.rs`:

```rust
use anyhow::{bail, Result};

pub async fn add(name: String, platform: &str, default_route: &str) -> Result<()> {
    if platform != "stub" {
        bail!(
            "platform '{platform}' not supported in Track C1 — only 'stub' is available. \
             Telegram lands in C2; send-from-any-app in C3."
        );
    }
    if default_route.trim().is_empty() {
        bail!("--default-route must be non-empty");
    }
    scaffold_stub_bridge(&name, default_route).await
}

pub(crate) async fn scaffold_stub_bridge(_name: &str, _default_route: &str) -> Result<()> {
    bail!("scaffold not yet implemented") // M-c1.6.2
}
```

In `mur-core/src/cmd/agent_companion/mod.rs` (or wherever `agent_companion` declares submodules — check via `ls mur-core/src/cmd/agent_companion/`), add `pub mod connector;`.

- [x] **Step 4: Build** — `cargo build -p mur-core`. PASS.

- [x] **Step 5: Commit** — `git add mur-core/src/cmd/agent_companion.rs mur-core/src/cmd/agent_companion/connector.rs mur-core/src/cmd/agent_companion/mod.rs && git commit -m "M-c1.6.1: wire 'mur agent companion connector add' subcommand"`

### Task M-c1.6.2: Stub bridge scaffold

- [x] **Step 1: Failing test** — `mur-core/tests/connector_add_stub.rs`:

```rust
use std::process::Command;
use tempfile::TempDir;

#[test]
fn stub_bridge_creates_expected_layout() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();

    let exe = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(exe)
        .args(["agent", "companion", "connector", "add", "stub_bridge",
               "--platform", "stub", "--default-route", "coach"])
        .env("MUR_HOME", &mur_home)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

    let dir = mur_home.join("agents/stub_bridge");
    assert!(dir.join("profile.yaml").exists());
    assert!(dir.join("routes.yaml").exists());
    assert!(dir.join("identity.key").exists());
    assert!(dir.join("identity.pub").exists());

    let p = std::fs::read_to_string(dir.join("profile.yaml")).unwrap();
    assert!(p.contains("llm:"));
    assert!(p.contains("mode: off"));

    let r = std::fs::read_to_string(dir.join("routes.yaml")).unwrap();
    assert!(r.contains("default_route: coach"));
}
```

- [x] **Step 2: Verify FAIL** — `cargo test -p mur-core --test connector_add_stub`.

- [x] **Step 3: Implement scaffold** — replace the stub `scaffold_stub_bridge`:

```rust
pub(crate) async fn scaffold_stub_bridge(name: &str, default_route: &str) -> Result<()> {
    use mur_common::identity::AgentIdentity;
    use mur_common::bridge::routes::BridgeRouteConfig;
    use std::path::PathBuf;

    mur_common::validate_agent_name(name)?;

    let mur_home = std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().expect("home").join(".mur"));
    let dir = mur_home.join("agents").join(name);
    if dir.exists() { bail!("agent dir already exists: {}", dir.display()); }
    std::fs::create_dir_all(&dir)?;

    // 1. identity
    let id = AgentIdentity::generate();
    id.save(&dir)?;

    // 2. routes
    let routes = BridgeRouteConfig {
        default_route: default_route.to_string(),
        routes: vec![],
    };
    std::fs::write(dir.join("routes.yaml"), serde_yaml_ng::to_string(&routes)?)?;

    // 3. profile — build typed AgentProfile + serde_yaml_ng emit (avoids string-format drift)
    let now = chrono::Utc::now().to_rfc3339();
    let pubkey = mur_common::identity::encode_pubkey(&id.verifying_key());
    let mut profile = mur_common::AgentProfile {
        // Start from any existing minimal-profile constructor or builder. If
        // mur_common doesn't expose one yet, deserialize from the round-trip
        // fixture at mur-common/src/agent.rs:702 then mutate fields below.
        // The exact constructor call depends on what helpers exist when this
        // plan is executed — pick the path of least resistance.
        ..serde_yaml_ng::from_str(include_str!(
            "../../../../mur-common/tests/fixtures/minimal_profile.yaml"
        )).expect("minimal_profile fixture parses")
    };
    profile.id = uuid::Uuid::now_v7().to_string();
    profile.name = name.to_string();
    profile.display_name = name.to_string();
    profile.identity.pubkey = pubkey.clone();
    profile.identity.algorithm = "ed25519".to_string();
    profile.identity.key_version = 0;
    profile.entitlements.llm.mode = mur_common::LlmMode::Off;
    profile.entitlements.network.outbound.mode = mur_common::agent::NetworkOutboundMode::Off;
    profile.trusted_peers = vec![];
    profile.created_at = now.clone();
    profile.updated_at = now;
    std::fs::write(dir.join("profile.yaml"), serde_yaml_ng::to_string(&profile)?)?;

    // 4. sys_prompt placeholder (LLM never reads it; schema requires it)
    std::fs::write(dir.join("sys_prompt.md"),
        "# Bridge sys_prompt\nThis agent is a bridge (llm.mode = off).\n")?;

    println!("✅ stub bridge '{name}' scaffolded at {}", dir.display());
    println!("   pubkey: {pubkey}");
    println!("   default_route: {default_route}");
    println!("   trusted_peers: []  ← user agent must add this bridge to its trusted_peers[]");
    Ok(())
}
```

Cross-check the inline YAML against `mur-common/src/agent.rs:702` to make sure all required fields are present. If the loader complains about missing fields, copy them from the round-trip test fixture there.

- [x] **Step 4: Verify PASS** — `cargo test -p mur-core --test connector_add_stub`.

- [x] **Step 5: Commit** — `git add mur-core/src/cmd/agent_companion/connector.rs mur-core/tests/connector_add_stub.rs && git commit -m "M-c1.6.2: stub bridge scaffold writes profile + routes + identity"`

### Task M-c1.6.3: Reject duplicate add + non-stub platform

- [x] **Step 1: Tests** — append to `mur-core/tests/connector_add_stub.rs`:

```rust
#[test]
fn unknown_platform_errors() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["agent", "companion", "connector", "add", "tg_bridge",
               "--platform", "telegram", "--default-route", "coach"])
        .env("MUR_HOME", &mur_home)
        .output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not supported in Track C1"));
}

#[test]
fn duplicate_add_errors() {
    let tmp = TempDir::new().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    let exe = env!("CARGO_BIN_EXE_mur");
    let mut go = || Command::new(exe)
        .args(["agent", "companion", "connector", "add", "stub_bridge",
               "--platform", "stub", "--default-route", "coach"])
        .env("MUR_HOME", &mur_home).output().unwrap();
    assert!(go().status.success());
    let dup = go();
    assert!(!dup.status.success());
    assert!(String::from_utf8_lossy(&dup.stderr).contains("already exists"));
}
```

- [x] **Step 2: Verify** — `cargo test -p mur-core --test connector_add_stub` (3 tests). PASS.

- [x] **Step 3: Commit** — `git add mur-core/tests/connector_add_stub.rs && git commit -m "M-c1.6.3: reject non-stub platform + dup-add"`

---

## M-c1.7 — E2E + cookbook + spec tick

### Task M-c1.7.1: E2E shell harness

- [x] **Step 1: Create** — `scripts/e2e/c1-bridge-roundtrip.sh`:

```bash
#!/usr/bin/env bash
# scripts/e2e/c1-bridge-roundtrip.sh — Track C1 stub-bridge round-trip.
#
# Acceptance gates:
#  1. Stub scaffold creates valid profile + identity + routes
#  2. User agent rejects unsigned envelopes
#  3. User agent accepts trusted-peer envelopes
#  4. AckTracker — offset advances only on 2xx
#  5. doctor surfaces bridge as `running`

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

echo "==> 1/4 build"
cargo build -p mur-core -p mur-agent-runtime --release --quiet

echo "==> 2/4 unit + integration tests"
cargo test --release -p mur-common bridge:: --quiet
cargo test --release -p mur-agent-runtime --quiet \
    --test bridge_envelope_signing \
    --test bridge_dedupe_sled \
    --test bridge_ack_tracker \
    --test bridge_beacon_degraded \
    --test bridge_llm_off_blocks

echo "==> 3/4 connector add + doctor"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export MUR_HOME="$TMP/.mur"
mkdir -p "$MUR_HOME/agents"

./target/release/mur agent companion connector add stub_bridge \
    --platform stub --default-route coach
test -f "$MUR_HOME/agents/stub_bridge/profile.yaml"
test -f "$MUR_HOME/agents/stub_bridge/routes.yaml"
test -f "$MUR_HOME/agents/stub_bridge/identity.pub"

echo '{"pid": 1}' > "$MUR_HOME/agents/stub_bridge/running.lock"
DOCTOR="$(./target/release/mur agent doctor --format all 2>&1 || true)"
echo "$DOCTOR" | grep -E "stub_bridge: (running|degraded)" \
    || { echo "FAIL: doctor missing bridges section"; echo "$DOCTOR"; exit 1; }

echo "==> 4/4 round-trip integration"
cargo test --release -p mur-agent-runtime --test bridge_roundtrip --quiet

echo "✅ Track C1 bridge round-trip E2E passed"
```

- [x] **Step 2: chmod** — `chmod +x scripts/e2e/c1-bridge-roundtrip.sh`.

- [x] **Step 3: Commit** — `git add scripts/e2e/c1-bridge-roundtrip.sh && git commit -m "M-c1.7.1: E2E shell harness"`

### Task M-c1.7.2: Round-trip integration test

- [x] **Step 1: Test** — `mur-agent-runtime/tests/bridge_roundtrip.rs`:

```rust
//! Track C1 acceptance: stub bridge → SignedEnvelope → user-agent verify
//! against trusted_peers[] → 2xx advances offset.
//! This exercises the bridge plumbing in-process; full supervisor spawn
//! is the shell harness's job.

use mur_agent_runtime::bridge::ack::AckTracker;
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use mur_agent_runtime::bridge::verify::verify_inbound_envelope;
use mur_common::bridge::envelope::sign_payload;
use mur_common::bridge::peer::TrustedPeer;
use mur_common::identity::{encode_pubkey, AgentIdentity};
use tempfile::TempDir;

#[test]
fn stub_bridge_full_loop() {
    let tmp = TempDir::new().unwrap();
    let bridge_id = AgentIdentity::generate();
    let trust = vec![TrustedPeer {
        pubkey_multibase: encode_pubkey(&bridge_id.verifying_key()),
        name: "stub_bridge".into(),
        key_version: None,
    }];
    let mut dedupe = DedupeStore::open(tmp.path(), "stub_bridge").unwrap();
    let mut tracker: AckTracker<u64> = AckTracker::new(0);

    for n in 1u64..=3 {
        let key = format!("msg-{n}");
        assert!(!dedupe.is_seen(&key).unwrap());
        dedupe.mark_seen(&key).unwrap();

        let inner = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "params": { "agent": "coach", "body": format!("hello #{n}") },
            "id": n,
        });
        let env = sign_payload(serde_json::to_vec(&inner).unwrap(), &bridge_id, 0);
        verify_inbound_envelope(&env, &trust).expect("verifies");
        tracker.start_pending(n);
        tracker.confirm();
    }
    assert_eq!(tracker.committed_offset(), 3);
}

#[test]
fn untrusted_attacker_rejected() {
    let attacker = AgentIdentity::generate();
    let trusted = AgentIdentity::generate();
    let trust = vec![TrustedPeer {
        pubkey_multibase: encode_pubkey(&trusted.verifying_key()),
        name: "stub".into(),
        key_version: None,
    }];
    let env = sign_payload(b"evil".to_vec(), &attacker, 0);
    assert!(matches!(
        verify_inbound_envelope(&env, &trust).unwrap_err(),
        mur_common::bridge::envelope::EnvelopeError::UntrustedPeer
    ));
}

#[test]
fn five_xx_keeps_offset() {
    let mut t: AckTracker<u64> = AckTracker::new(10);
    t.start_pending(20);
    t.reject();
    assert_eq!(t.committed_offset(), 10);
}
```

- [x] **Step 2: Verify** — `cargo test -p mur-agent-runtime --test bridge_roundtrip` (3 tests). PASS.

- [x] **Step 3: Commit** — `git add mur-agent-runtime/tests/bridge_roundtrip.rs && git commit -m "M-c1.7.2: stub-bridge full-loop integration test"`

### Task M-c1.7.3: Cookbook

- [x] **Step 1: Create** — `docs/cookbook/c1-a2a-bridge.md`:

```markdown
# Track C1 — A2A Bridge Architecture

> A chat-platform bridge is **a small, dumb mur agent** with `entitlements.llm.mode = off`. It signs every outbound A2A envelope; the user agent pins the bridge's pubkey in `profile.yaml.trusted_peers[]` and rejects everything else.

Concrete platforms (Telegram → C2, send-from-any-app → C3) build on this pattern.

## Why a bridge is a mur agent

| Alternative | Rejected because |
|---|---|
| Library linked into user agent | Couples Slack outage to therapy |
| Python sidecar that pokes user-agent HTTP API | Re-implements auth, secrets, telemetry, lifecycle |
| Smart bridge w/ LLM triage | +800 ms; social-engineerable; breaks 99.99% target |

So a bridge is just another P0a runtime — `mur_agent_<platform>_inbound` — with `llm.mode = off`, its own Ed25519 identity, and the same `running.lock` + telemetry + permissions infra as any other agent.

## Wire shape

```rust
pub struct SignedEnvelope {
    pub payload: Vec<u8>,                  // canonical-JSON A2A JsonRpcRequest
    pub sig: Vec<u8>,                      // 64-byte Ed25519
    pub key_version: u32,
    pub bridge_pubkey_multibase: String,
}
```

Verification runs **regardless of transport** — Unix socket has no peer auth; Noise XK only proves *some* peer's identity, not authorization to claim the bridge role.

## `routes.yaml`

```yaml
default_route: coach
routes:
  - match: { platform: telegram, mention: "@coach" }
    agent: coach
  - match: { platform: telegram, chat_id: "12345" }
    agent: therapist
  - match: { platform: telegram, chat_id: "67890" }
    agent: coach
    fanout: [coach, journal_agent]
```

Precedence: mention > chat_id > `default_route`. No LLM in routing.

## Behaviour summary

- **Dedupe** `(bridge_id, platform_msg_id)` → sled, 7-day TTL, lazy sweep every 256 lookups. (`DedupeStore`)
- **ACK** Bridge advances its platform offset only on 2xx. On 5xx the offset stays pinned; dedupe drops the re-fetched duplicates. (`AckTracker`)
- **Heartbeat** `telemetry/bridge_alive` every 30 s. `mur agent doctor` shows `degraded` once `running.lock` mtime > 90 s. (`BridgeBeacon`, `bridge_status_for_peer`)

## Scaffolding a stub bridge (testing only)

```bash
mur agent companion connector add stub_bridge \
    --platform stub \
    --default-route coach
```

Writes `~/.mur/agents/stub_bridge/{profile.yaml,routes.yaml,identity.{key,pub},sys_prompt.md}`. The user agent must then add the bridge pubkey to its own `trusted_peers[]` (manual YAML edit for now; CLI sugar lands in C2).

## NOT in C1

- No Telegram / Slack / Discord / IMAP → C2 / C3
- No `send-from-any-app` UX → C3
- No CLI sugar for `add-trusted-peer` → C2
```

- [x] **Step 2: Commit** — `git add docs/cookbook/c1-a2a-bridge.md && git commit -m "M-c1.7.3: Track C1 cookbook"`

### Task M-c1.7.4: Spec tick

- [x] **Step 1: Locate footer** — `grep -n "Quiet hours / proactive policy" docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` (line 542).

- [x] **Step 2: Append** — after line 542, insert:

```markdown

#### Acceptance status

- §5.1 — bridge-as-mur-agent pattern  ✅ landed (track-c1 PR cascade; see `docs/cookbook/c1-a2a-bridge.md`)
- §5.2 — `routes.yaml` + precedence  ✅ landed (`mur_common::bridge::routes::BridgeRouteConfig::resolve`)
- §5.3 — dedupe / heartbeat / signing / trust  ✅ landed:
  - dedupe → `mur_agent_runtime::bridge::dedupe::DedupeStore` (sled, 7-day TTL)
  - heartbeat → `BridgeBeacon` (30 s) + `bridge_status_for_peer` (90 s degraded threshold)
  - ACK → `mur_agent_runtime::bridge::ack::AckTracker`
  - signing → `mur_common::bridge::envelope::SignedEnvelope` + `verify_inbound_envelope`
  - trust → `AgentProfile.trusted_peers: Vec<TrustedPeer>`
  - llm-block → `entitlements.llm.mode = off`

E2E: `scripts/e2e/c1-bridge-roundtrip.sh`. Concrete platforms ship in C2 / C3.
```

- [x] **Step 3: Run E2E** — `./scripts/e2e/c1-bridge-roundtrip.sh`. Expect `✅ Track C1 bridge round-trip E2E passed`.

Common failure modes:
- profile YAML schema mismatch → check `entitlements.llm.mode` indent
- doctor doesn't list bridges → confirm `MUR_HOME` reaches `collect_bridge_statuses`
- sled doesn't persist → confirm Drop runs (sled v0.34 flushes on Drop)

- [x] **Step 4: Commit** — `git add docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md && git commit -m "M-c1.7.4: tick §5.1 / §5.2 / §5.3 acceptance footer"`

---

## Self-review

### Spec coverage

| Spec section | Implementing tasks |
|---|---|
| §5.1 ASCII diagram + "dumb plumbing" + `entitlements.llm = none` | M-c1.0.1, M-c1.0.2, M-c1.6.1, M-c1.6.2, M-c1.7.3 |
| §5.1 BusyBox-style symlink (inherits P0a; unchanged) | (P0a — out of scope) |
| §5.1 Bot token / OAuth never crosses A2A or MCP boundary | (inherits P0a secrets/keychain — concrete platforms ship in C2) |
| §5.2 routes.yaml schema | M-c1.1.1, M-c1.1.2 |
| §5.2 precedence (mention > chat_id > default) | M-c1.1.3, M-c1.1.4 |
| §5.2 fanout opt-in multicast | M-c1.1.3 (`fanout_returns_full_list`) |
| §5.2 "no LLM triage in routing" | M-c1.0.2 (block) + M-c1.1.2 (resolver is pure data) |
| §5.3 dedupe key + persistence + 7-day TTL | M-c1.2.1 → M-c1.2.4 |
| §5.3 ACK ordering | M-c1.5.1 → M-c1.5.3 |
| §5.3 heartbeat (30 s + 90 s degraded) | M-c1.4.1 → M-c1.4.4 |
| §5.3 envelope signing | M-c1.3.1, M-c1.3.2 |
| §5.3 trust pin + reject unsigned/wrong-key | M-c1.3.3, M-c1.3.4, M-c1.3.5 |
| §5.3 platform identity informational only | (enforced by construction — `RouteMatch` and `verify_inbound_envelope` never read `metadata.platform`; cookbook documents this) |
| §5.3 quiet-hours / proactive in user agent | (out of scope — `companion::earned_permission` already lives in user agents; we add nothing to bridges that would change this) |

### Placeholder scan

No "TBD" / "implement later" / "similar to Task N" / unparameterized error handling. The forward references in M-c1.2.2 (`sweep_expired` returns `Ok(0)`) and M-c1.6.1 (`scaffold_stub_bridge` bails) are explicit and labeled with "implemented in M-c1.X.Y" pointers — the next task replaces them.

### Type-name consistency

Cross-task type names: `SignedEnvelope`, `EnvelopeError`, `TrustedPeer`, `BridgeRouteConfig`, `RouteEntry`, `RouteMatch`, `Resolution`, `InboundMessage`, `LlmEntitlement`, `LlmMode`, `DedupeStore`, `DedupeError`, `AckTracker<T>`, `BridgeBeacon`, `BridgePeerStatus`, `bridge_status_for_peer`, `verify_inbound_envelope`, `sign_payload`, `make_alive_payload`, `METHOD_BRIDGE_ALIVE`, `DEGRADED_AFTER_SECS`, `BridgeSummary`, `collect_bridge_statuses`, `scaffold_stub_bridge`. All consistent across tasks.
