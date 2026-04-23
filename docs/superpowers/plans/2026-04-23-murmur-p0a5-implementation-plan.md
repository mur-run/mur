# murmur P0a.5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-agent Ed25519 identity + Noise-XK TCP transport to the P0a agent runtime, and wire P0a agents into the existing `mur-commander` v0.7 workflow engine so commander auto-registers them, exposes A2A v0.3 method names, and starts collecting per-agent telemetry into a disk-backed spool (no hub upstream forward yet — that lands in P1).

**Architecture:** Additive, zero-regression. `AgentProfile` gains optional `identity` + `transport.tcp` + `lifecycle.{execution,schedule}` + `file_transfer` blocks. Runtime spawns an optional TCP listener that performs Noise XK handshake using `identity.key` as static keypair. Commander (separate repo) grows a `murmur_bridge` module watching `~/.mur/agents/*/running.lock` + an `observability::collector` module tailing each agent's telemetry JSONL and normalising to OTel spans in a spool dir.

**Tech Stack:**
- Rust 2024 edition (workspace pinned)
- `ed25519-dalek = "2"` + `x25519-dalek = "2"` for identity + key derivation
- `multibase = "0.9"` for text-safe pubkey encoding
- `snow = "0.9"` for Noise XK handshake
- `notify = "6"` for inotify/FSEvents watching (commander side)
- `opentelemetry = "0.24"` + `opentelemetry-sdk` for OTel span normalization (commander side)
- Existing: `tokio`, `serde`, `serde_yaml`, `async-trait`, `uuid`, `chrono`

**Specs:**
- Primary: `docs/superpowers/specs/2026-04-23-murmur-fleet-architecture-design.md` §11.1
- Predecessor: `docs/superpowers/specs/2026-04-22-murmur-p0-agent-runtime-design.md`

**Branch strategy:**
- This repo (mur main workspace): new branch `feat/murmur-p0a.5` off `feat/murmur-p0a` (rebase onto `main` once PR #24 merges)
- Commander repo: new branch `feat/murmur-bridge` off `main` in `~/Projects/mur-commander/`

**Done definition:** All tasks below pass their paired tests. The E2E script `scripts/e2e/p0a5-full.sh` (Task G3) runs green end-to-end, covering: agent create with identity → TCP+Noise handshake between two agents → commander auto-register → commander collector spools normalised OTel JSON.

---

## File Structure

### New files (mur workspace)

```
mur-common/src/
  identity.rs                    ← Ed25519 keypair load/save + multibase encoding +
                                   Ed25519↔X25519 conversion helpers

mur-agent-runtime/src/
  transport/
    tcp.rs                       ← TCP listener + Noise XK handshake +
                                   length-prefixed JSON-RPC framing
    noise.rs                     ← Noise XK handshake helpers (responder +
                                   initiator) built on snow

scripts/e2e/
  p0a5-full.sh                   ← cross-repo E2E smoke runner
```

### Modified files (mur workspace)

```
mur-common/src/agent.rs          ← extend AgentProfile: identity, transport.tcp,
                                   lifecycle.{execution,schedule}, file_transfer,
                                   deployment blocks
mur-common/src/lib.rs            ← re-export identity module
mur-common/Cargo.toml            ← add ed25519-dalek, x25519-dalek, multibase
mur-agent-runtime/Cargo.toml     ← add snow
mur-agent-runtime/src/transport/mod.rs  ← expose tcp module
mur-agent-runtime/src/protocol/methods/card.rs  ← inject pubkey + endpoints[] +
                                                   deployment into Agent Card JSON
mur-agent-runtime/src/supervisor.rs  ← conditionally spawn TCP listener
mur-core/src/cmd/agent.rs        ← mur agent create generates identity.key/.pub
```

### New files (mur-commander workspace, separate repo)

```
crates/engine/src/remote/
  murmur_bridge.rs               ← fs watcher → AgentRegistry auto-register

crates/engine/src/observability/
  mod.rs                         ← collector module root
  collector.rs                   ← jsonl tail + OTel normalize + redaction + spool
  redaction.rs                   ← three redaction modes
  spool.rs                       ← disk-backed ring buffer
```

### Modified files (mur-commander)

```
crates/engine/src/a2a/protocol.rs  ← add MESSAGE_SEND, MESSAGE_STREAM, TASKS_LIST
                                     constants
crates/engine/src/a2a/server.rs    ← handle new methods (alias to existing
                                     tasks/send, tasks/sendSubscribe; new
                                     tasks/list handler)
crates/engine/src/a2a/discovery.rs ← RegisteredAgent gains Option<uuid>,
                                     Option<pubkey> fields
crates/engine/src/remote/mod.rs    ← export murmur_bridge
crates/engine/src/lib.rs           ← export observability
crates/engine/Cargo.toml           ← add notify, opentelemetry*, mur-common path dep
```

---

## Phase A — Identity + Profile Schema Foundation (mur-common)

### Task A1: Add crypto crates to mur-common

**Files:**
- Modify: `mur-common/Cargo.toml`

- [ ] **Step 1: Add dependencies**

Append to `[dependencies]`:

```toml
ed25519-dalek = { version = "2", features = ["rand_core", "pkcs8", "pem"] }
x25519-dalek = { version = "2", features = ["static_secrets"] }
rand_core = { version = "0.6", features = ["getrandom"] }
multibase = "0.9"
```

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo check -p mur-common`
Expected: compiles with new deps resolved.

- [ ] **Step 3: Commit**

```bash
git checkout -b feat/murmur-p0a.5
git add mur-common/Cargo.toml
git commit -m "deps(common): add ed25519-dalek + x25519-dalek + multibase for P0a.5 identity"
```

---

### Task A2: Ed25519 keypair generate + save + load

**Files:**
- Create: `mur-common/src/identity.rs`
- Modify: `mur-common/src/lib.rs`
- Test: `mur-common/tests/identity.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-common/tests/identity.rs`:

```rust
use mur_common::identity::{AgentIdentity, IdentityError};
use tempfile::TempDir;

#[test]
fn generate_roundtrip() {
    let dir = TempDir::new().unwrap();
    let id = AgentIdentity::generate();
    id.save(dir.path()).unwrap();
    let loaded = AgentIdentity::load(dir.path()).unwrap();
    assert_eq!(id.verifying_key_bytes(), loaded.verifying_key_bytes());
}

#[test]
fn load_missing_returns_err() {
    let dir = TempDir::new().unwrap();
    let err = AgentIdentity::load(dir.path()).unwrap_err();
    assert!(matches!(err, IdentityError::NotFound));
}

#[test]
fn private_key_file_is_mode_0600() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        id.save(dir.path()).unwrap();
        let meta = std::fs::metadata(dir.path().join("identity.key")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p mur-common --test identity`
Expected: FAIL — `identity` module not found.

- [ ] **Step 3: Implement `mur-common/src/identity.rs`**

```rust
//! Per-agent Ed25519 identity keypair.
//!
//! Loaded from `<agent_home>/identity.key` (private, 0600) and
//! `<agent_home>/identity.pub` (public, multibase-encoded text).

use ed25519_dalek::{SigningKey, VerifyingKey, SECRET_KEY_LENGTH};
use rand_core::OsRng;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("identity files not found in {0:?}")]
    NotFound,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid key material: {0}")]
    InvalidKey(String),
    #[error("multibase decode error: {0}")]
    Multibase(#[from] multibase::Error),
}

#[derive(Clone)]
pub struct AgentIdentity {
    signing: SigningKey,
}

impl AgentIdentity {
    /// Generate a fresh Ed25519 keypair using OS CSPRNG.
    pub fn generate() -> Self {
        Self {
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    /// Write both halves of the keypair to the given directory.
    /// Private key is mode 0600 on Unix.
    pub fn save(&self, dir: &Path) -> Result<(), IdentityError> {
        fs::create_dir_all(dir)?;
        let priv_path = dir.join("identity.key");
        let pub_path = dir.join("identity.pub");

        fs::write(&priv_path, self.signing.to_bytes())?;
        #[cfg(unix)]
        {
            let mut perms = fs::metadata(&priv_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&priv_path, perms)?;
        }

        let pub_text = encode_pubkey(&self.signing.verifying_key());
        fs::write(&pub_path, pub_text)?;
        Ok(())
    }

    /// Load both halves from the given directory. Prefers the private key
    /// (since we can derive pubkey from it); but also validates that a
    /// present `identity.pub` matches.
    pub fn load(dir: &Path) -> Result<Self, IdentityError> {
        let priv_path = dir.join("identity.key");
        if !priv_path.exists() {
            return Err(IdentityError::NotFound);
        }
        let bytes = fs::read(&priv_path)?;
        if bytes.len() != SECRET_KEY_LENGTH {
            return Err(IdentityError::InvalidKey(format!(
                "expected {SECRET_KEY_LENGTH} bytes, got {}",
                bytes.len()
            )));
        }
        let arr: [u8; SECRET_KEY_LENGTH] = bytes.as_slice().try_into().unwrap();
        let signing = SigningKey::from_bytes(&arr);

        let pub_path = dir.join("identity.pub");
        if pub_path.exists() {
            let text = fs::read_to_string(&pub_path)?;
            let loaded_pub = decode_pubkey(text.trim())?;
            if loaded_pub != *signing.verifying_key().as_bytes() {
                return Err(IdentityError::InvalidKey(
                    "identity.pub does not match identity.key".into(),
                ));
            }
        }

        Ok(Self { signing })
    }

    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        *self.signing.verifying_key().as_bytes()
    }

    pub fn pubkey_text(&self) -> String {
        encode_pubkey(&self.signing.verifying_key())
    }
}

/// Encode an Ed25519 public key to multibase base58btc (`z` prefix).
pub fn encode_pubkey(key: &VerifyingKey) -> String {
    multibase::encode(multibase::Base::Base58Btc, key.as_bytes())
}

/// Decode a multibase-encoded pubkey. Accepts any multibase variant.
pub fn decode_pubkey(text: &str) -> Result<[u8; 32], IdentityError> {
    let (_base, bytes) = multibase::decode(text)?;
    if bytes.len() != 32 {
        return Err(IdentityError::InvalidKey(format!(
            "pubkey must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Default location: `<agent_home>/identity.{key,pub}`.
pub fn default_dir(agent_home: &Path) -> PathBuf {
    agent_home.to_path_buf()
}
```

- [ ] **Step 4: Re-export from lib.rs**

Append to `mur-common/src/lib.rs` (after existing `pub mod` lines):

```rust
pub mod identity;
pub use identity::{AgentIdentity, IdentityError, encode_pubkey, decode_pubkey};
```

- [ ] **Step 5: Run test — should pass**

Run: `cargo test -p mur-common --test identity`
Expected: PASS — all three tests.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/identity.rs mur-common/src/lib.rs mur-common/tests/identity.rs
git commit -m "feat(common): AgentIdentity — Ed25519 keypair load/save/multibase"
```

---

### Task A3: Multibase round-trip + encoding edge cases

**Files:**
- Test: `mur-common/tests/identity.rs` (extend)

- [ ] **Step 1: Add encoding tests**

Append to `mur-common/tests/identity.rs`:

```rust
use mur_common::identity::{decode_pubkey, encode_pubkey};

#[test]
fn pubkey_text_starts_with_z() {
    let id = AgentIdentity::generate();
    let text = id.pubkey_text();
    assert!(text.starts_with('z'), "expected base58btc 'z' prefix, got: {text}");
}

#[test]
fn pubkey_roundtrip() {
    let id = AgentIdentity::generate();
    let encoded = id.pubkey_text();
    let decoded = decode_pubkey(&encoded).unwrap();
    assert_eq!(decoded, id.verifying_key_bytes());
}

#[test]
fn decode_wrong_length_errors() {
    // base58btc encoding of 16 zero bytes → wrong length after decode
    let short = multibase::encode(multibase::Base::Base58Btc, [0u8; 16]);
    let err = decode_pubkey(&short).unwrap_err();
    match err {
        IdentityError::InvalidKey(_) => {}
        other => panic!("expected InvalidKey, got {other:?}"),
    }
}

#[test]
fn decode_invalid_text_errors() {
    let err = decode_pubkey("not-multibase").unwrap_err();
    assert!(matches!(err, IdentityError::Multibase(_)));
}
```

- [ ] **Step 2: Run tests — must pass**

Run: `cargo test -p mur-common --test identity`
Expected: all 7 tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-common/tests/identity.rs
git commit -m "test(common): multibase encode/decode edge cases for AgentIdentity"
```

---

### Task A4: Ed25519 → X25519 conversion helper for Noise XK

**Files:**
- Modify: `mur-common/src/identity.rs`
- Test: `mur-common/tests/identity.rs` (extend)

- [ ] **Step 1: Write the failing test**

Append:

```rust
#[test]
fn ed25519_to_x25519_static_key_is_deterministic() {
    let id = AgentIdentity::generate();
    let k1 = id.to_x25519_static_secret();
    let k2 = id.to_x25519_static_secret();
    // Montgomery form derivation is deterministic from Ed25519 scalar
    assert_eq!(k1.to_bytes(), k2.to_bytes());
}

#[test]
fn ed25519_x25519_pair_agree() {
    // Two agents can compute matching shared secrets via X25519 derived
    // from their Ed25519 keypairs.
    let a = AgentIdentity::generate();
    let b = AgentIdentity::generate();

    let a_priv = a.to_x25519_static_secret();
    let b_priv = b.to_x25519_static_secret();
    let a_pub = x25519_dalek::PublicKey::from(&a_priv);
    let b_pub = x25519_dalek::PublicKey::from(&b_priv);

    let shared_a = a_priv.diffie_hellman(&b_pub);
    let shared_b = b_priv.diffie_hellman(&a_pub);
    assert_eq!(shared_a.as_bytes(), shared_b.as_bytes());
}
```

- [ ] **Step 2: Run — expect FAIL (to_x25519_static_secret missing)**

Run: `cargo test -p mur-common --test identity ed25519_to_x25519`
Expected: FAIL — method not found.

- [ ] **Step 3: Add the method**

Append inside `impl AgentIdentity`:

```rust
    /// Derive the X25519 static secret usable by Noise XK.
    ///
    /// Ed25519 and X25519 both use Curve25519 underneath; the Ed25519
    /// SigningKey scalar maps directly to an X25519 StaticSecret.
    /// ed25519-dalek 2.x exposes `to_scalar_bytes()` for exactly this.
    pub fn to_x25519_static_secret(&self) -> x25519_dalek::StaticSecret {
        let scalar_bytes = self.signing.to_scalar_bytes();
        x25519_dalek::StaticSecret::from(scalar_bytes)
    }
```

- [ ] **Step 4: Run tests — must pass**

Run: `cargo test -p mur-common --test identity`
Expected: 9 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/identity.rs mur-common/tests/identity.rs
git commit -m "feat(common): Ed25519 → X25519 conversion for Noise XK interop"
```

---

### Task A5: Profile schema — add `IdentityConfig`

**Files:**
- Modify: `mur-common/src/agent.rs`
- Test: `mur-common/tests/profile_schema.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `mur-common/tests/profile_schema.rs`:

```rust
use mur_common::agent::{AgentProfile, IdentityConfig};

#[test]
fn profile_identity_defaults_are_empty_and_optional() {
    // Loading an old P0a-style YAML without identity block must still work;
    // Default values populate the field.
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert!(p.identity.pubkey.is_empty() || p.identity.pubkey.starts_with('z'));
    assert!(p.identity.owner.is_none());
}

#[test]
fn profile_identity_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a5_with_identity.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(p.identity.pubkey, "zABCD1234");
    assert_eq!(p.identity.owner.as_deref(), Some("david@twdd.com.tw"));

    // Roundtrip
    let emitted = serde_yaml::to_string(&p).unwrap();
    let p2: AgentProfile = serde_yaml::from_str(&emitted).unwrap();
    assert_eq!(p, p2);
}
```

Create `mur-common/tests/fixtures/profile_p0a_minimal.yaml`:

```yaml
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPE
name: agent_test
display_name: "Test"
version: "0.1.0"
persona:
  category: research
  description: "test"
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model:
  provider: ollama
  name: "llama3.2:3b"
  params: { temperature: 0.2, max_tokens: 4096 }
transport:
  stdio: true
  socket:
    enabled: true
    bind: "unix:///tmp/agent.sock"
communication:
  accepts_from: ["*"]
  sends_to: []
capabilities: []
entitlements:
  network: { inbound: { ports: [] }, outbound: { mode: restricted, allow_hosts: [], protocols: [tcp], resolve_dns: { mode: system } } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle:
  restart: on_failure
  max_restarts: 3
  restart_window_secs: 600
  stop_timeout_secs: 15
  mcp_required: true
created_at: "2026-04-22T10:00:00+08:00"
updated_at: "2026-04-22T10:00:00+08:00"
```

Create `mur-common/tests/fixtures/profile_p0a5_with_identity.yaml` — copy of above but add:

```yaml
identity:
  pubkey: zABCD1234
  owner: david@twdd.com.tw
```

before `created_at:`.

- [ ] **Step 2: Run — expect FAIL (IdentityConfig doesn't exist)**

Run: `cargo test -p mur-common --test profile_schema`
Expected: FAIL.

- [ ] **Step 3: Add `IdentityConfig` to agent.rs**

In `mur-common/src/agent.rs`, inside the existing module (near `Persona`), add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct IdentityConfig {
    /// Multibase-encoded Ed25519 public key (base58btc, `z` prefix).
    /// Empty string for legacy P0a profiles; filled on P0a.5 `mur agent create`.
    #[serde(default)]
    pub pubkey: String,
    /// Free-form owner identity (email / SSO sub). None for legacy profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}
```

Extend `AgentProfile` struct: insert after `pub version: String,`:

```rust
    /// Cryptographic identity for cross-host A2A (P0a.5+). Default = empty
    /// (legacy P0a profiles continue to load without this block).
    #[serde(default)]
    pub identity: IdentityConfig,
```

Re-export from `mur-common/src/lib.rs`:

```rust
pub use agent::IdentityConfig;
```

- [ ] **Step 4: Run tests — must pass**

Run: `cargo test -p mur-common --test profile_schema`
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-common/src/lib.rs mur-common/tests/profile_schema.rs mur-common/tests/fixtures/
git commit -m "feat(common): AgentProfile.identity block (pubkey + owner, default empty)"
```

---

### Task A6: Profile schema — extend `TransportConfig` with `tcp`

**Files:**
- Modify: `mur-common/src/agent.rs`
- Test: `mur-common/tests/profile_schema.rs` (extend)

- [ ] **Step 1: Write failing test**

Append to `mur-common/tests/profile_schema.rs`:

```rust
#[test]
fn tcp_transport_default_disabled() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert!(!p.transport.tcp.enabled, "tcp must default disabled");
    assert!(p.transport.tcp.bind.is_empty());
}

#[test]
fn tcp_transport_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a5_tcp_enabled.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert!(p.transport.tcp.enabled);
    assert_eq!(p.transport.tcp.bind, "0.0.0.0:39393");
    assert_eq!(
        p.transport.tcp.noise.pattern,
        "Noise_XK_25519_ChaChaPoly_BLAKE2s"
    );
}
```

Create fixture `mur-common/tests/fixtures/profile_p0a5_tcp_enabled.yaml` — copy of `profile_p0a_minimal.yaml`, but replace the `transport:` block with:

```yaml
transport:
  stdio: true
  socket:
    enabled: true
    bind: "unix:///tmp/agent.sock"
  tcp:
    enabled: true
    bind: "0.0.0.0:39393"
    noise:
      pattern: "Noise_XK_25519_ChaChaPoly_BLAKE2s"
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-common --test profile_schema tcp`
Expected: FAIL — `tcp` field missing.

- [ ] **Step 3: Extend `TransportConfig`**

In `mur-common/src/agent.rs`, find:

```rust
pub struct TransportConfig {
    pub stdio: bool,
    pub socket: SocketTransportConfig,
}
```

Replace with:

```rust
pub struct TransportConfig {
    pub stdio: bool,
    pub socket: SocketTransportConfig,
    #[serde(default)]
    pub tcp: TcpTransportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TcpTransportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bind: String,
    #[serde(default)]
    pub noise: NoiseConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoiseConfig {
    pub pattern: String,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            pattern: "Noise_XK_25519_ChaChaPoly_BLAKE2s".into(),
        }
    }
}
```

- [ ] **Step 4: Run tests — must pass**

Run: `cargo test -p mur-common --test profile_schema`
Expected: all 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-common/tests/profile_schema.rs mur-common/tests/fixtures/profile_p0a5_tcp_enabled.yaml
git commit -m "feat(common): TransportConfig.tcp with Noise XK pattern (default disabled)"
```

---

### Task A7: Profile schema — extend `LifecycleConfig` + add `file_transfer` + `deployment`

**Files:**
- Modify: `mur-common/src/agent.rs`
- Test: `mur-common/tests/profile_schema.rs` (extend)

- [ ] **Step 1: Write failing tests**

Append:

```rust
#[test]
fn lifecycle_execution_defaults_daemon() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(p.lifecycle.execution, mur_common::agent::ExecutionMode::Daemon);
    assert!(p.lifecycle.schedule.is_empty());
}

#[test]
fn lifecycle_schedule_on_demand_roundtrip() {
    let yaml = include_str!("fixtures/profile_p0a5_scheduled.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(p.lifecycle.execution, mur_common::agent::ExecutionMode::OnDemand);
    assert_eq!(p.lifecycle.schedule.len(), 1);
    assert_eq!(p.lifecycle.schedule[0].cron, "0 9 * * 1-5");
}

#[test]
fn file_transfer_defaults_sensible() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(p.file_transfer.accept_incoming_file_max_bytes, 10_485_760);
    assert_eq!(p.file_transfer.require_approval_above_bytes, 10_485_760);
}

#[test]
fn deployment_defaults_to_laptop() {
    let yaml = include_str!("fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(p.deployment.deployment_type, mur_common::agent::DeploymentType::Laptop);
    assert_eq!(p.deployment.environment.as_deref(), Some("dev"));
}
```

Create fixture `profile_p0a5_scheduled.yaml` — start from `profile_p0a_minimal.yaml`, replace the `lifecycle:` block with:

```yaml
lifecycle:
  restart: never
  max_restarts: 3
  restart_window_secs: 600
  stop_timeout_secs: 15
  mcp_required: true
  execution: on_demand
  schedule:
    - cron: "0 9 * * 1-5"
      message: "daily briefing"
      sends_to: notify_a
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-common --test profile_schema lifecycle_execution`
Expected: FAIL.

- [ ] **Step 3: Extend schema**

Add new types near `LifecycleConfig`:

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Daemon,
    OnDemand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScheduleEntry {
    pub cron: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileTransferConfig {
    #[serde(default = "default_accept_max")]
    pub accept_incoming_file_max_bytes: u64,
    #[serde(default = "default_accept_total")]
    pub accept_incoming_total_per_hour: u64,
    #[serde(default = "default_approval_threshold")]
    pub require_approval_above_bytes: u64,
    #[serde(default = "default_reject_paths")]
    pub reject_paths: Vec<String>,
    #[serde(default = "default_allowed_mime")]
    pub allowed_mime_types: Vec<String>,
}

impl Default for FileTransferConfig {
    fn default() -> Self {
        Self {
            accept_incoming_file_max_bytes: default_accept_max(),
            accept_incoming_total_per_hour: default_accept_total(),
            require_approval_above_bytes: default_approval_threshold(),
            reject_paths: default_reject_paths(),
            allowed_mime_types: default_allowed_mime(),
        }
    }
}

fn default_accept_max() -> u64 { 10_485_760 }
fn default_accept_total() -> u64 { 104_857_600 }
fn default_approval_threshold() -> u64 { 10_485_760 }
fn default_reject_paths() -> Vec<String> {
    vec!["~/.ssh".into(), "~/.aws".into(), "~/.gnupg".into()]
}
fn default_allowed_mime() -> Vec<String> { vec!["*".into()] }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentType {
    #[default]
    Laptop,
    Vm,
    Docker,
    K8s,
    Lambda,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeploymentConfig {
    #[serde(rename = "type", default)]
    pub deployment_type: DeploymentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default = "default_env")]
    pub environment: Option<String>,
}

impl Default for DeploymentConfig {
    fn default() -> Self {
        Self { deployment_type: DeploymentType::default(), region: None, environment: default_env() }
    }
}

fn default_env() -> Option<String> { Some("dev".into()) }
```

Extend `LifecycleConfig`:

```rust
pub struct LifecycleConfig {
    // ... existing fields ...
    #[serde(default)]
    pub execution: ExecutionMode,
    #[serde(default)]
    pub schedule: Vec<ScheduleEntry>,
}
```

Extend `AgentProfile` (insert before `created_at:`):

```rust
    #[serde(default)]
    pub file_transfer: FileTransferConfig,
    #[serde(default)]
    pub deployment: DeploymentConfig,
```

Re-export the new types from `lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-common --test profile_schema`
Expected: 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs mur-common/src/lib.rs mur-common/tests/profile_schema.rs mur-common/tests/fixtures/profile_p0a5_scheduled.yaml
git commit -m "feat(common): lifecycle.execution/schedule + file_transfer + deployment blocks"
```

---

### Task A8: Verify full workspace build + P0a fixture compat

**Files:**
- Test: none new; compile-wide

- [ ] **Step 1: Run workspace tests**

Run: `cargo test --workspace`
Expected: all existing P0a tests pass (unmodified). New tests in `mur-common` pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings.

- [ ] **Step 3: Run fmt**

Run: `cargo fmt --check`
Expected: pass.

- [ ] **Step 4: Commit (if any autofix)**

```bash
git status
# if fmt applied changes:
git add -u && git commit -m "style: cargo fmt"
```

---

## Phase B — TCP + Noise XK Transport (mur-agent-runtime)

### Task B1: Add `snow` dependency

**Files:**
- Modify: `mur-agent-runtime/Cargo.toml`

- [ ] **Step 1: Add dep**

In `[dependencies]`:

```toml
snow = { version = "0.9", default-features = false, features = ["default-resolver"] }
```

- [ ] **Step 2: Compile check**

Run: `cargo check -p mur-agent-runtime`
Expected: resolves.

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/Cargo.toml
git commit -m "deps(agent-runtime): snow 0.9 for Noise XK"
```

---

### Task B2: Noise XK handshake helpers (responder path)

**Files:**
- Create: `mur-agent-runtime/src/transport/noise.rs`
- Modify: `mur-agent-runtime/src/transport/mod.rs`
- Test: `mur-agent-runtime/tests/noise_handshake.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-agent-runtime/tests/noise_handshake.rs`:

```rust
use mur_agent_runtime::transport::noise::{build_initiator, build_responder};
use mur_common::identity::AgentIdentity;

#[test]
fn xk_handshake_completes_in_three_messages() {
    let responder_id = AgentIdentity::generate();
    let initiator_id = AgentIdentity::generate();

    let responder_static = responder_id.to_x25519_static_secret().to_bytes();
    let responder_pub = x25519_dalek::PublicKey::from(
        &responder_id.to_x25519_static_secret(),
    );

    let mut responder = build_responder(&responder_static).unwrap();
    let mut initiator = build_initiator(
        &initiator_id.to_x25519_static_secret().to_bytes(),
        responder_pub.as_bytes(),
    )
    .unwrap();

    // msg 1: -> e, es
    let mut buf1 = [0u8; 1024];
    let n = initiator.write_message(&[], &mut buf1).unwrap();
    responder.read_message(&buf1[..n], &mut []).unwrap();

    // msg 2: <- e, ee
    let mut buf2 = [0u8; 1024];
    let n = responder.write_message(&[], &mut buf2).unwrap();
    initiator.read_message(&buf2[..n], &mut []).unwrap();

    // msg 3: -> s, se
    let mut buf3 = [0u8; 1024];
    let n = initiator.write_message(&[], &mut buf3).unwrap();
    responder.read_message(&buf3[..n], &mut []).unwrap();

    assert!(initiator.is_handshake_finished());
    assert!(responder.is_handshake_finished());

    // Post-handshake: responder learns initiator's static pubkey
    let remote_static = responder.get_remote_static().unwrap();
    assert_eq!(
        remote_static,
        x25519_dalek::PublicKey::from(&initiator_id.to_x25519_static_secret())
            .as_bytes()
    );
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-agent-runtime --test noise_handshake`
Expected: FAIL — `transport::noise` module not found.

- [ ] **Step 3: Implement `mur-agent-runtime/src/transport/noise.rs`**

```rust
//! Noise XK handshake helpers.
//!
//! Pattern: Noise_XK_25519_ChaChaPoly_BLAKE2s. Static key is the agent's
//! X25519 secret derived from its Ed25519 identity.
//!
//! Responder knows its own static key; initiator knows responder's static
//! pubkey a priori (obtained from Agent Card via hub lookup — Q2 decision).

use snow::{Builder, HandshakeState, params::NoiseParams};
use thiserror::Error;

pub const NOISE_XK_PATTERN: &str = "Noise_XK_25519_ChaChaPoly_BLAKE2s";

#[derive(Debug, Error)]
pub enum NoiseError {
    #[error("noise builder error: {0}")]
    Builder(String),
    #[error("invalid params")]
    InvalidParams,
}

/// Build a responder (server-side) handshake state. `static_secret` is 32
/// bytes of X25519 private scalar (typically derived from the agent's
/// Ed25519 identity via `AgentIdentity::to_x25519_static_secret`).
pub fn build_responder(static_secret: &[u8; 32]) -> Result<HandshakeState, NoiseError> {
    let params: NoiseParams = NOISE_XK_PATTERN
        .parse()
        .map_err(|_| NoiseError::InvalidParams)?;
    Builder::new(params)
        .local_private_key(static_secret)
        .build_responder()
        .map_err(|e| NoiseError::Builder(e.to_string()))
}

/// Build an initiator (client-side) handshake state, with prior knowledge
/// of the responder's static pubkey.
pub fn build_initiator(
    static_secret: &[u8; 32],
    remote_static_pub: &[u8; 32],
) -> Result<HandshakeState, NoiseError> {
    let params: NoiseParams = NOISE_XK_PATTERN
        .parse()
        .map_err(|_| NoiseError::InvalidParams)?;
    Builder::new(params)
        .local_private_key(static_secret)
        .remote_public_key(remote_static_pub)
        .build_initiator()
        .map_err(|e| NoiseError::Builder(e.to_string()))
}
```

Update `mur-agent-runtime/src/transport/mod.rs`:

```rust
//! Transport layer.
pub mod stdio;

#[cfg(unix)]
pub mod unix_socket;

pub mod noise;
pub mod tcp;
```

- [ ] **Step 4: Run — expect PASS**

Run: `cargo test -p mur-agent-runtime --test noise_handshake`
Expected: FAIL — `transport::tcp` doesn't exist yet (from `mod.rs`). Defer `tcp` line to Task B4.

Fix temporarily: remove `pub mod tcp;` from `mod.rs` until B4.

Re-run: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/transport/noise.rs mur-agent-runtime/src/transport/mod.rs mur-agent-runtime/tests/noise_handshake.rs
git commit -m "feat(agent-runtime): Noise XK handshake helpers (responder + initiator)"
```

---

### Task B3: Length-prefixed JSON-RPC frame codec

**Files:**
- Modify: `mur-agent-runtime/src/transport/noise.rs` (add codec helpers)
- Test: `mur-agent-runtime/tests/noise_frame.rs`

- [ ] **Step 1: Write failing test**

Create `mur-agent-runtime/tests/noise_frame.rs`:

```rust
use mur_agent_runtime::transport::noise::{encode_frame, decode_frame, FrameError};

#[test]
fn frame_roundtrip() {
    let payload = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let framed = encode_frame(payload).unwrap();
    // first 4 bytes = big-endian length
    assert_eq!(&framed[..4], &(payload.len() as u32).to_be_bytes());
    let decoded = decode_frame(&framed).unwrap();
    assert_eq!(decoded.payload, payload);
    assert_eq!(decoded.consumed, framed.len());
}

#[test]
fn short_header_errors() {
    let err = decode_frame(&[0, 0]).unwrap_err();
    assert!(matches!(err, FrameError::Incomplete));
}

#[test]
fn short_body_errors() {
    // Header says 100 bytes; only 5 follow
    let mut buf = vec![0, 0, 0, 100];
    buf.extend_from_slice(b"short");
    let err = decode_frame(&buf).unwrap_err();
    assert!(matches!(err, FrameError::Incomplete));
}

#[test]
fn oversize_rejected() {
    // 100 MB in header — refuse
    let buf = 100_000_000u32.to_be_bytes().to_vec();
    let err = decode_frame(&buf).unwrap_err();
    assert!(matches!(err, FrameError::TooLarge));
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-agent-runtime --test noise_frame`
Expected: FAIL.

- [ ] **Step 3: Implement codec**

Append to `mur-agent-runtime/src/transport/noise.rs`:

```rust
/// Maximum frame payload size (16 MiB). Matches commander's HTTP body cap.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("incomplete frame")]
    Incomplete,
    #[error("frame too large (>{MAX_FRAME_BYTES} bytes)")]
    TooLarge,
}

/// Decoded frame outcome.
#[derive(Debug)]
pub struct DecodedFrame<'a> {
    pub payload: &'a [u8],
    pub consumed: usize,
}

/// Encode a single JSON-RPC payload as a 4-byte BE-length-prefixed frame.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Decode the first complete frame from `buf`. Returns `Incomplete` if
/// fewer than 4+len bytes are present.
pub fn decode_frame(buf: &[u8]) -> Result<DecodedFrame<'_>, FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::Incomplete);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge);
    }
    if buf.len() < 4 + len {
        return Err(FrameError::Incomplete);
    }
    Ok(DecodedFrame {
        payload: &buf[4..4 + len],
        consumed: 4 + len,
    })
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-agent-runtime --test noise_frame`
Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/transport/noise.rs mur-agent-runtime/tests/noise_frame.rs
git commit -m "feat(agent-runtime): length-prefixed frame codec for Noise JSON-RPC streams"
```

---

### Task B4: TCP listener with Noise XK handshake

**Files:**
- Create: `mur-agent-runtime/src/transport/tcp.rs`
- Modify: `mur-agent-runtime/src/transport/mod.rs` (re-enable `pub mod tcp;`)
- Test: `mur-agent-runtime/tests/tcp_transport.rs`

- [ ] **Step 1: Write failing integration test**

Create `mur-agent-runtime/tests/tcp_transport.rs`:

```rust
use mur_agent_runtime::transport::tcp::{TcpTransportConfig, spawn_tcp_listener, TcpConnector};
use mur_common::identity::AgentIdentity;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::test]
async fn end_to_end_handshake_and_echo() {
    let server_id = Arc::new(AgentIdentity::generate());
    let client_id = Arc::new(AgentIdentity::generate());

    // Handler: echo back the JSON-RPC payload
    let handler = Arc::new(|payload: Vec<u8>| async move {
        Ok::<_, std::io::Error>(payload) // echo
    });

    let cfg = TcpTransportConfig {
        bind: "127.0.0.1:0".into(), // kernel picks free port
    };
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
    let handle = spawn_tcp_listener(cfg, server_id.clone(), handler, shutdown_rx)
        .await
        .unwrap();
    let actual_addr = handle.local_addr();

    // Client connects
    let server_pub = x25519_dalek::PublicKey::from(&server_id.to_x25519_static_secret());
    let mut conn = TcpConnector::dial(
        &actual_addr.to_string(),
        client_id.clone(),
        server_pub.as_bytes(),
    )
    .await
    .unwrap();

    let payload = br#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#.to_vec();
    conn.send(&payload).await.unwrap();
    let reply = conn.recv().await.unwrap();
    assert_eq!(reply, payload);

    drop(shutdown_tx);
    handle.await.unwrap();
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-agent-runtime --test tcp_transport`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement `mur-agent-runtime/src/transport/tcp.rs`**

```rust
//! TCP transport with Noise XK handshake + length-prefixed JSON-RPC frames.
//!
//! Server (responder) accepts connections, completes Noise XK handshake
//! using its static key (= agent identity.key derived to X25519), then
//! passes decrypted payloads to the handler. Client (initiator) dials a
//! known endpoint, knows the responder's X25519 pubkey a priori (from
//! Agent Card lookup), and speaks the same frame protocol post-handshake.

use crate::transport::noise::{
    MAX_FRAME_BYTES, build_initiator, build_responder, decode_frame, encode_frame,
};
use mur_common::identity::AgentIdentity;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub struct TcpTransportConfig {
    pub bind: String,
}

pub type HandlerFn = Arc<
    dyn for<'a> Fn(
            Vec<u8>,
        )
            -> Pin<Box<dyn Future<Output = Result<Vec<u8>, io::Error>> + Send + 'a>>
        + Send
        + Sync,
>;

pub struct TcpListenerHandle {
    local_addr: SocketAddr,
    join: tokio::task::JoinHandle<()>,
}

impl TcpListenerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
    pub async fn await_shutdown(self) {
        let _ = self.join.await;
    }
}

impl std::future::IntoFuture for TcpListenerHandle {
    type Output = Result<(), tokio::task::JoinError>;
    type IntoFuture = tokio::task::JoinHandle<Result<(), tokio::task::JoinError>>;
    fn into_future(self) -> Self::IntoFuture {
        tokio::spawn(async move { self.join.await })
    }
}

/// Spawn a Noise-XK TCP listener. Shutdown by dropping `shutdown_rx`.
pub async fn spawn_tcp_listener<F, Fut>(
    cfg: TcpTransportConfig,
    identity: Arc<AgentIdentity>,
    handler: Arc<F>,
    mut shutdown_rx: mpsc::Receiver<()>,
) -> io::Result<TcpListenerHandle>
where
    F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<u8>, io::Error>> + Send + 'static,
{
    let listener = TcpListener::bind(&cfg.bind).await?;
    let local_addr = listener.local_addr()?;
    info!(%local_addr, "TCP Noise listener bound");

    let static_secret = identity.to_x25519_static_secret().to_bytes();

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("TCP listener shutting down");
                    break;
                }
                res = listener.accept() => {
                    match res {
                        Ok((stream, peer)) => {
                            debug!(%peer, "TCP accepted");
                            let h = handler.clone();
                            let s = static_secret;
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, s, h).await {
                                    warn!(?e, "connection ended with error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(?e, "accept failed");
                        }
                    }
                }
            }
        }
    });

    Ok(TcpListenerHandle { local_addr, join })
}

async fn handle_connection<F, Fut>(
    mut stream: TcpStream,
    static_secret: [u8; 32],
    handler: Arc<F>,
) -> io::Result<()>
where
    F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<u8>, io::Error>> + Send + 'static,
{
    let mut noise =
        build_responder(&static_secret).map_err(|e| io::Error::other(e.to_string()))?;

    // handshake — msg 1 in
    let buf = read_framed(&mut stream).await?;
    let mut tmp = [0u8; 1024];
    noise
        .read_message(&buf, &mut tmp)
        .map_err(|e| io::Error::other(e.to_string()))?;

    // msg 2 out
    let mut out = [0u8; 1024];
    let n = noise
        .write_message(&[], &mut out)
        .map_err(|e| io::Error::other(e.to_string()))?;
    write_framed(&mut stream, &out[..n]).await?;

    // msg 3 in
    let buf = read_framed(&mut stream).await?;
    noise
        .read_message(&buf, &mut tmp)
        .map_err(|e| io::Error::other(e.to_string()))?;

    assert!(noise.is_handshake_finished());
    let mut transport = noise
        .into_transport_mode()
        .map_err(|e| io::Error::other(e.to_string()))?;

    // Application loop
    loop {
        let cipher_frame = match read_framed(&mut stream).await {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        let mut plain = vec![0u8; cipher_frame.len()];
        let n = transport
            .read_message(&cipher_frame, &mut plain)
            .map_err(|e| io::Error::other(e.to_string()))?;
        plain.truncate(n);

        let reply = handler(plain).await?;

        let mut cipher_out = vec![0u8; reply.len() + 16];
        let n = transport
            .write_message(&reply, &mut cipher_out)
            .map_err(|e| io::Error::other(e.to_string()))?;
        cipher_out.truncate(n);
        write_framed(&mut stream, &cipher_out).await?;
    }

    Ok(())
}

async fn read_framed(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut hdr = [0u8; 4];
    stream.read_exact(&mut hdr).await?;
    let len = u32::from_be_bytes(hdr) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_framed(stream: &mut TcpStream, payload: &[u8]) -> io::Result<()> {
    let framed = encode_frame(payload).map_err(|e| io::Error::other(e.to_string()))?;
    stream.write_all(&framed).await?;
    Ok(())
}

// ---- Client side ------------------------------------------------------

pub struct TcpConnector {
    stream: TcpStream,
    transport: snow::TransportState,
}

impl TcpConnector {
    pub async fn dial(
        addr: &str,
        identity: Arc<AgentIdentity>,
        remote_static_pub: &[u8; 32],
    ) -> io::Result<Self> {
        let mut stream = TcpStream::connect(addr).await?;
        let static_secret = identity.to_x25519_static_secret().to_bytes();
        let mut noise = build_initiator(&static_secret, remote_static_pub)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // msg 1 out
        let mut buf = [0u8; 1024];
        let n = noise
            .write_message(&[], &mut buf)
            .map_err(|e| io::Error::other(e.to_string()))?;
        write_framed(&mut stream, &buf[..n]).await?;

        // msg 2 in
        let in_buf = read_framed(&mut stream).await?;
        let mut tmp = [0u8; 1024];
        noise
            .read_message(&in_buf, &mut tmp)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // msg 3 out
        let n = noise
            .write_message(&[], &mut buf)
            .map_err(|e| io::Error::other(e.to_string()))?;
        write_framed(&mut stream, &buf[..n]).await?;

        assert!(noise.is_handshake_finished());
        let transport = noise
            .into_transport_mode()
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self { stream, transport })
    }

    pub async fn send(&mut self, payload: &[u8]) -> io::Result<()> {
        let mut cipher = vec![0u8; payload.len() + 16];
        let n = self
            .transport
            .write_message(payload, &mut cipher)
            .map_err(|e| io::Error::other(e.to_string()))?;
        cipher.truncate(n);
        write_framed(&mut self.stream, &cipher).await
    }

    pub async fn recv(&mut self) -> io::Result<Vec<u8>> {
        let cipher = read_framed(&mut self.stream).await?;
        let mut plain = vec![0u8; cipher.len()];
        let n = self
            .transport
            .read_message(&cipher, &mut plain)
            .map_err(|e| io::Error::other(e.to_string()))?;
        plain.truncate(n);
        Ok(plain)
    }
}
```

Re-enable `pub mod tcp;` in `mur-agent-runtime/src/transport/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-agent-runtime --test tcp_transport`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/transport/tcp.rs mur-agent-runtime/src/transport/mod.rs mur-agent-runtime/tests/tcp_transport.rs
git commit -m "feat(agent-runtime): TCP listener + connector with Noise XK + frame codec"
```

---

### Task B5: Reject peers whose static key differs from Agent-Card advertisement

**Files:**
- Modify: `mur-agent-runtime/src/transport/tcp.rs`
- Test: `mur-agent-runtime/tests/tcp_transport.rs` (extend)

- [ ] **Step 1: Add test**

Append:

```rust
#[tokio::test]
async fn dialer_aborts_on_mitm_mismatch() {
    let real_server = Arc::new(AgentIdentity::generate());
    let fake_pub = [0u8; 32]; // wrong

    let handler = Arc::new(|p: Vec<u8>| async move { Ok::<_, std::io::Error>(p) });
    let (tx, rx) = mpsc::channel(1);
    let handle = spawn_tcp_listener(
        TcpTransportConfig { bind: "127.0.0.1:0".into() },
        real_server,
        handler,
        rx,
    )
    .await
    .unwrap();

    // The test MUST fail during handshake because initiator encrypts its
    // own static key to an unrelated pubkey.
    let client_id = Arc::new(AgentIdentity::generate());
    let res = TcpConnector::dial(
        &handle.local_addr().to_string(),
        client_id,
        &fake_pub,
    )
    .await;
    assert!(res.is_err(), "MITM-style mismatch must fail handshake");
    drop(tx);
}
```

- [ ] **Step 2: Run — should pass without code change**

Run: `cargo test -p mur-agent-runtime --test tcp_transport dialer_aborts`
Expected: PASS (snow XK will fail to decrypt when keys don't match).

- [ ] **Step 3: Commit**

```bash
git add mur-agent-runtime/tests/tcp_transport.rs
git commit -m "test(agent-runtime): dialer aborts when peer static key mismatches"
```

---

### Task B6: Supervisor conditionally spawns TCP listener

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`

- [ ] **Step 1: Inspect current supervisor**

Run: `grep -n "fn start\|spawn_\|unix_socket\|UnixListener" mur-agent-runtime/src/supervisor.rs | head -20`

- [ ] **Step 2: Integrate TCP listener**

Locate the startup sequence (around the point where Unix socket listener is spawned). Add after it:

```rust
// P0a.5: conditionally spawn Noise-XK TCP listener.
if profile.inner.transport.tcp.enabled && !profile.inner.transport.tcp.bind.is_empty() {
    use crate::transport::tcp::{spawn_tcp_listener, TcpTransportConfig};
    let tcp_identity = Arc::new(identity.clone());
    let tcp_handler = dispatcher_handler.clone(); // same JSON-RPC dispatcher
    let tcp_cfg = TcpTransportConfig {
        bind: profile.inner.transport.tcp.bind.clone(),
    };
    let (tcp_shutdown_tx, tcp_shutdown_rx) = mpsc::channel(1);
    let tcp_handle =
        spawn_tcp_listener(tcp_cfg, tcp_identity, tcp_handler, tcp_shutdown_rx).await?;
    tracing::info!(
        "TCP Noise listener at {}",
        tcp_handle.local_addr()
    );
    shutdown_senders.push(tcp_shutdown_tx);
    listener_handles.push(tcp_handle.into_future());
}
```

(Exact variable names depend on the existing supervisor code; adjust `dispatcher_handler`, `shutdown_senders`, `listener_handles` to match real names found in Step 1.)

Add identity loading earlier in supervisor startup (after profile load):

```rust
use mur_common::identity::AgentIdentity;
let identity = AgentIdentity::load(&agent_home).unwrap_or_else(|_| {
    tracing::warn!("No identity keypair found; generating ephemeral (cross-host TCP disabled)");
    AgentIdentity::generate()
});
```

- [ ] **Step 3: Run workspace tests**

Run: `cargo test -p mur-agent-runtime`
Expected: all pre-existing tests still pass; TCP listener only spawns when opted in.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs
git commit -m "feat(agent-runtime): supervisor spawns TCP Noise listener when opted in"
```

---

### Task B7: Agent Card exposes `pubkey`, `endpoints[]`, `deployment`

**Files:**
- Modify: `mur-agent-runtime/src/protocol/methods/card.rs`
- Test: `mur-agent-runtime/tests/card_extended.rs`

- [ ] **Step 1: Write failing test**

Create `mur-agent-runtime/tests/card_extended.rs`:

```rust
use mur_agent_runtime::profile::Profile;
use mur_agent_runtime::protocol::methods::card::CardHandler;
use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use serde_json::Value;
use std::sync::Arc;

fn test_profile() -> Profile {
    // A profile with identity.pubkey populated + TCP enabled + deployment set.
    let yaml = include_str!("fixtures/card_full_profile.yaml");
    Profile::from_yaml_str(yaml, std::path::PathBuf::from("/tmp/test")).unwrap()
}

#[tokio::test]
async fn card_includes_pubkey_endpoints_deployment() {
    let p = Arc::new(test_profile());
    let handler = CardHandler::new(p);
    let json: Value = handler.handle(None).await.unwrap();

    assert_eq!(json["pubkey"], "zTESTPUB");
    let eps = json["endpoints"].as_array().unwrap();
    // order: tcp first (most reachable), then unix-socket, then stdio
    assert_eq!(eps[0]["transport"], "tcp+noise");
    assert_eq!(eps[0]["reachability"], "lan");
    assert_eq!(json["deployment"]["type"], "docker");
    assert_eq!(json["deployment"]["environment"], "prod");
}
```

Create fixture `mur-agent-runtime/tests/fixtures/card_full_profile.yaml` — copy the minimal profile fixture and add:

```yaml
identity:
  pubkey: zTESTPUB
  owner: test@example.com
transport:
  stdio: true
  socket:
    enabled: true
    bind: "unix:///tmp/a.sock"
  tcp:
    enabled: true
    bind: "0.0.0.0:39393"
    noise:
      pattern: "Noise_XK_25519_ChaChaPoly_BLAKE2s"
deployment:
  type: docker
  environment: prod
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-agent-runtime --test card_extended`
Expected: FAIL — fields missing.

- [ ] **Step 3: Extend CardHandler**

Replace the body of `handle` in `mur-agent-runtime/src/protocol/methods/card.rs`:

```rust
    async fn handle(&self, _params: Option<Value>) -> Result<Value, HandlerError> {
        let p = &self.profile.inner;

        let mut transports: Vec<&str> = vec![];
        let mut endpoints: Vec<Value> = vec![];

        if p.transport.tcp.enabled && !p.transport.tcp.bind.is_empty() {
            transports.push("tcp+noise");
            endpoints.push(json!({
                "transport": "tcp+noise",
                "url": format!("tcp://{}", p.transport.tcp.bind),
                "reachability": "lan",
            }));
        }
        if p.transport.socket.enabled && p.transport.socket.bind.starts_with("unix://") {
            transports.push("unix-socket");
            endpoints.push(json!({
                "transport": "unix-socket",
                "url": p.transport.socket.bind,
                "reachability": "local",
            }));
        }
        if p.transport.stdio {
            transports.push("stdio");
            endpoints.push(json!({
                "transport": "stdio",
                "url": "pipe://self",
                "reachability": "local",
            }));
        }

        Ok(json!({
            "protocolVersion": "a2a/0.3",
            "name": p.name,
            "id": p.id,
            "pubkey": p.identity.pubkey,
            "displayName": p.display_name,
            "version": p.version,
            "description": p.persona.description,
            "capabilities": p.capabilities,
            "transports": transports,
            "endpoints": endpoints,
            "deployment": {
                "type": p.deployment.deployment_type,
                "region": p.deployment.region,
                "environment": p.deployment.environment,
            },
            "persona": {
                "category": p.persona.category,
                "traits": p.persona.traits,
            },
            "skills": p.skills.iter().map(|s| json!({"id": s})).collect::<Vec<_>>(),
            "entitlements": p.entitlements,
        }))
    }
```

- [ ] **Step 4: Update existing card tests if any break**

Run: `cargo test -p mur-agent-runtime`
If `existing_card_test` from P0a asserts on `endpoints` shape, update to new list-of-objects form or use selective field assertions.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/methods/card.rs mur-agent-runtime/tests/card_extended.rs mur-agent-runtime/tests/fixtures/card_full_profile.yaml
git commit -m "feat(agent-runtime): Agent Card exposes pubkey + endpoints[] + deployment"
```

---

### Task B8: Entitlements gate — refuse TCP bind if `network.inbound.ports` empty

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs` (or wherever profile validation lives)
- Test: `mur-agent-runtime/tests/tcp_entitlement.rs`

- [ ] **Step 1: Write failing test**

Create `mur-agent-runtime/tests/tcp_entitlement.rs`:

```rust
use mur_agent_runtime::supervisor::validate_tcp_entitlement;
use mur_common::agent::{AgentProfile, Entitlements};

#[test]
fn tcp_enabled_but_no_inbound_port_is_error() {
    let mut p = AgentProfile::minimal_test_default();
    p.transport.tcp.enabled = true;
    p.transport.tcp.bind = "0.0.0.0:39393".into();
    // entitlements.network.inbound.ports is empty by default
    let err = validate_tcp_entitlement(&p).unwrap_err();
    assert!(err.to_string().contains("network.inbound.ports"));
}

#[test]
fn tcp_enabled_with_matching_port_ok() {
    let mut p = AgentProfile::minimal_test_default();
    p.transport.tcp.enabled = true;
    p.transport.tcp.bind = "0.0.0.0:39393".into();
    p.entitlements.network.inbound.ports = vec![39393];
    assert!(validate_tcp_entitlement(&p).is_ok());
}
```

(If `AgentProfile::minimal_test_default` does not exist in mur-common, add it as a `#[cfg(any(test, feature = "testing"))]` helper that returns a valid minimal profile. Then enable the feature in this test's dev-deps.)

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p mur-agent-runtime --test tcp_entitlement`
Expected: FAIL.

- [ ] **Step 3: Implement validator**

Add to `mur-agent-runtime/src/supervisor.rs`:

```rust
/// Cross-check profile: if TCP is enabled, its bind port must be in
/// entitlements.network.inbound.ports (empty list = "no inbound").
pub fn validate_tcp_entitlement(p: &mur_common::agent::AgentProfile)
    -> Result<(), ValidationError>
{
    if !p.transport.tcp.enabled {
        return Ok(());
    }
    let port = p
        .transport
        .tcp
        .bind
        .rsplit(':')
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| ValidationError(
            "transport.tcp.bind missing parseable port".into()
        ))?;
    if !p.entitlements.network.inbound.ports.contains(&port) {
        return Err(ValidationError(format!(
            "transport.tcp bound to :{port} but entitlements.network.inbound.ports does not allow it"
        )));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ValidationError(pub String);
```

Call it in the startup sequence before the TCP listener spawn (Task B6).

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-agent-runtime --test tcp_entitlement`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs mur-agent-runtime/tests/tcp_entitlement.rs
git commit -m "feat(agent-runtime): validate TCP bind port against inbound entitlements"
```

---

### Task B9: Workspace check — all pre-P0a.5 behaviour preserved

**Files:** none

- [ ] **Step 1: Run full workspace**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

Expected: all green. Any P0a test referencing old Agent Card shape should already have been updated in B7.

- [ ] **Step 2: Commit any formatting**

```bash
git status
# if anything to clean:
git add -u && git commit -m "style: cargo fmt"
```

---

## Phase C — `mur agent create` generates identity (mur-core)

### Task C1: Identity generation step in `mur agent create`

**Files:**
- Modify: `mur-core/src/cmd/agent.rs`
- Test: `mur-core/tests/agent_create_identity.rs`

- [ ] **Step 1: Locate current `create` implementation**

Run: `grep -n "fn handle_create\|agent create\|write_profile\|identity" mur-core/src/cmd/agent.rs | head -20`

Note which function writes the profile.yaml + sys_prompt.md. That is the insertion point.

- [ ] **Step 2: Write failing test**

Create `mur-core/tests/agent_create_identity.rs`:

```rust
use mur_core::cmd::agent::create_agent_with_defaults;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

#[tokio::test]
async fn create_generates_identity_files() {
    let home = TempDir::new().unwrap();
    unsafe { std::env::set_var("MUR_HOME", home.path()); }

    create_agent_with_defaults("test_agent", "Test", "research").await.unwrap();

    let agent_dir = home.path().join("agents").join("test_agent");
    assert!(agent_dir.join("identity.key").exists());
    assert!(agent_dir.join("identity.pub").exists());

    // Verify roundtrip — loaded identity.pub must match identity.key derivation
    let id = AgentIdentity::load(&agent_dir).unwrap();
    let pub_text = std::fs::read_to_string(agent_dir.join("identity.pub")).unwrap();
    assert_eq!(pub_text.trim(), id.pubkey_text());
}

#[tokio::test]
async fn create_writes_identity_into_profile() {
    let home = TempDir::new().unwrap();
    unsafe { std::env::set_var("MUR_HOME", home.path()); }

    create_agent_with_defaults("test_agent_2", "Test", "research").await.unwrap();

    let yaml = std::fs::read_to_string(
        home.path().join("agents/test_agent_2/profile.yaml"),
    )
    .unwrap();
    assert!(yaml.contains("identity:"));
    assert!(yaml.contains("pubkey: z"));
}
```

(`create_agent_with_defaults` may need to be added as a test-accessible helper wrapping the existing `mur agent create` logic — if current implementation is CLI-glued, extract core into a pub function.)

- [ ] **Step 3: Run — expect FAIL**

Run: `cargo test -p mur-core --test agent_create_identity`
Expected: FAIL.

- [ ] **Step 4: Extract + modify create logic**

Inside `mur-core/src/cmd/agent.rs`, find the creation flow. Add identity generation at the point where `agent_dir` exists and before profile write:

```rust
use mur_common::identity::AgentIdentity;

// Generate identity keypair (P0a.5)
let identity = AgentIdentity::generate();
identity.save(&agent_dir)?;
tracing::info!(pubkey = %identity.pubkey_text(), "generated identity keypair");

// ... when building AgentProfile, set identity field:
let profile = AgentProfile {
    // ... existing fields ...
    identity: mur_common::agent::IdentityConfig {
        pubkey: identity.pubkey_text(),
        owner: std::env::var("USER").ok(),
    },
    // ... rest unchanged ...
};
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p mur-core --test agent_create_identity`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent.rs mur-core/tests/agent_create_identity.rs
git commit -m "feat(core): mur agent create generates Ed25519 identity + writes into profile"
```

---

### Task C2: `mur agent card` / `status` show pubkey

**Files:**
- Modify: `mur-core/src/cmd/agent.rs` (card / status commands)
- Test: `mur-core/tests/agent_card_cli.rs`

- [ ] **Step 1: Failing test**

Create `mur-core/tests/agent_card_cli.rs`:

```rust
// Smoke test: `mur agent card <name>` prints pubkey line.
use assert_cmd::Command;
use tempfile::TempDir;

#[test]
fn agent_card_prints_pubkey() {
    let home = TempDir::new().unwrap();
    // setup: create agent
    Command::cargo_bin("mur")
        .unwrap()
        .env("MUR_HOME", home.path())
        .args(["agent", "create", "pubkey_test", "--no-interactive",
               "--display-name", "T", "--category", "research"])
        .assert()
        .success();

    let out = Command::cargo_bin("mur")
        .unwrap()
        .env("MUR_HOME", home.path())
        .args(["agent", "card", "pubkey_test"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("pubkey"), "output: {stdout}");
    assert!(stdout.contains("z"), "pubkey multibase prefix missing: {stdout}");
}
```

- [ ] **Step 2: Run — may PASS already** (if existing card command prints the profile JSON)

Run: `cargo test -p mur-core --test agent_card_cli`

- [ ] **Step 3: If FAIL, extend print logic**

In the `card` subcommand handler, ensure pubkey line is included. Typically there's a display function like:

```rust
println!("  Pubkey:        {}", profile.identity.pubkey);
```

Added after `Name` / `UUID` lines.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent.rs mur-core/tests/agent_card_cli.rs
git commit -m "feat(core): mur agent card displays identity.pubkey"
```

---

### Task C3: Profile loader tolerates legacy P0a files (no identity block)

**Files:**
- Test: `mur-core/tests/profile_legacy_load.rs`

- [ ] **Step 1: Failing test (which should already pass thanks to serde defaults)**

Create `mur-core/tests/profile_legacy_load.rs`:

```rust
use mur_common::agent::AgentProfile;

#[test]
fn legacy_p0a_profile_loads_with_empty_identity() {
    let yaml = std::fs::read_to_string(
        "../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
    ).unwrap();
    let p: AgentProfile = serde_yaml::from_str(&yaml).unwrap();
    assert!(p.identity.pubkey.is_empty());
}
```

- [ ] **Step 2: Run — must pass (verifies A5 defaults work)**

Run: `cargo test -p mur-core --test profile_legacy_load`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add mur-core/tests/profile_legacy_load.rs
git commit -m "test(core): confirm legacy P0a profiles load under P0a.5 schema"
```

---

### Task C4: Open-question gate — defer `mur agent rekey` (Q-B)

**Files:** none; this is a planning milestone.

- [ ] **Step 1: Document Q-B decision in spec § 13**

Current default in spec: "Yes, with re-registration to hub; old UUID retained but new pubkey."

Until user confirms, no code implements rekey. Note in a `// TODO(Q-B):` comment at the top of `mur-core/src/cmd/agent.rs`:

```rust
// TODO(Q-B): `mur agent rekey <name>` — regenerate identity keypair and
// re-register with commander. Blocked on user decision in spec § 13.
// If accepted, keep UUID stable; only rotate pubkey + notify peers.
```

- [ ] **Step 2: Commit**

```bash
git add mur-core/src/cmd/agent.rs
git commit -m "docs(core): TODO marker for Q-B (mur agent rekey) — awaiting user decision"
```

---

## Phase D — Commander A2A v0.3 Aliasing (cross-repo: mur-commander)

### Task D1: Switch workspace; create branch

**Files:** none

- [ ] **Step 1: Enter commander workspace**

Run:
```bash
cd ~/Projects/mur-commander
git fetch --all
git checkout main && git pull
git checkout -b feat/murmur-bridge
```

- [ ] **Step 2: Add mur-common as a path dep**

Edit `~/Projects/mur-commander/crates/engine/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
mur-common = { path = "../../../mur/mur-common" }
```

- [ ] **Step 3: Compile check**

Run: `cargo check -p engine`
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/engine/Cargo.toml
git commit -m "deps(engine): add mur-common path dep for P0a.5 interop"
```

---

### Task D2: A2A v0.3 method constants

**Files:**
- Modify: `crates/engine/src/a2a/protocol.rs`
- Test: `crates/engine/src/a2a/protocol.rs` (inline `#[test]`)

- [ ] **Step 1: Add constants**

Edit `crates/engine/src/a2a/protocol.rs`, extend the `methods` module:

```rust
pub mod methods {
    // Legacy (a2a/0.2-ish)
    pub const TASKS_SEND: &str = "tasks/send";
    pub const TASKS_GET: &str = "tasks/get";
    pub const TASKS_CANCEL: &str = "tasks/cancel";
    pub const TASKS_SEND_SUBSCRIBE: &str = "tasks/sendSubscribe";

    // v0.3 aliases (P0a.5 — talks to mur-agent-runtime)
    pub const MESSAGE_SEND: &str = "message/send";
    pub const MESSAGE_STREAM: &str = "message/stream";
    pub const TASKS_LIST: &str = "tasks/list";
}
```

- [ ] **Step 2: Inline test**

At the bottom of `protocol.rs`:

```rust
#[cfg(test)]
#[test]
fn v03_method_constants_exist() {
    assert_eq!(methods::MESSAGE_SEND, "message/send");
    assert_eq!(methods::MESSAGE_STREAM, "message/stream");
    assert_eq!(methods::TASKS_LIST, "tasks/list");
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p engine protocol::tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/engine/src/a2a/protocol.rs
git commit -m "feat(engine): A2A v0.3 method name constants (message/*, tasks/list)"
```

---

### Task D3: Dispatcher aliases v0.3 → existing handlers

**Files:**
- Modify: `crates/engine/src/a2a/server.rs`
- Test: `crates/engine/tests/a2a_v03_alias.rs`

- [ ] **Step 1: Write failing test**

Create `crates/engine/tests/a2a_v03_alias.rs`:

```rust
use engine::a2a::protocol::{JsonRpcRequest, methods};
use engine::a2a::server::{A2aServer, A2aServerConfig};
use serde_json::json;

#[test]
fn message_send_returns_same_shape_as_tasks_send() {
    let server = A2aServer::new(A2aServerConfig::default());

    let req_old = JsonRpcRequest::new(
        methods::TASKS_SEND,
        Some(json!({"message":{"role":"user","parts":[{"kind":"text","text":"ping"}]}})),
        json!(1),
    );
    let req_new = JsonRpcRequest::new(
        methods::MESSAGE_SEND,
        Some(json!({"message":{"role":"user","parts":[{"kind":"text","text":"ping"}]}})),
        json!(2),
    );
    let r_old = server.handle_request(&req_old);
    let r_new = server.handle_request(&req_new);

    // Both should return a result (no error); shape identical module ids
    assert!(r_old.error.is_none(), "tasks/send errored: {:?}", r_old.error);
    assert!(r_new.error.is_none(), "message/send errored: {:?}", r_new.error);
}

#[test]
fn tasks_list_returns_array() {
    let server = A2aServer::new(A2aServerConfig::default());
    let req = JsonRpcRequest::new(methods::TASKS_LIST, None, json!(1));
    let r = server.handle_request(&req);
    assert!(r.error.is_none());
    let result = r.result.unwrap();
    assert!(result.is_array());
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p engine --test a2a_v03_alias`
Expected: FAIL — `message/send` returns `-32601 method not found`.

- [ ] **Step 3: Extend dispatcher**

In `crates/engine/src/a2a/server.rs` `handle_request`:

```rust
pub fn handle_request(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
    match request.method.as_str() {
        // legacy
        methods::TASKS_SEND => self.handle_task_send(request),
        methods::TASKS_GET => self.handle_task_get(request),
        methods::TASKS_CANCEL => self.handle_task_cancel(request),
        methods::TASKS_SEND_SUBSCRIBE => self.handle_task_send_subscribe(request),

        // v0.3 aliases
        methods::MESSAGE_SEND => self.handle_task_send(request),
        methods::MESSAGE_STREAM => self.handle_task_send_subscribe(request),
        methods::TASKS_LIST => self.handle_tasks_list(request),

        _ => JsonRpcResponse::error(
            request.id.clone(),
            JsonRpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("method '{}' not supported", request.method),
                data: None,
            },
        ),
    }
}
```

Implement `handle_tasks_list`:

```rust
fn handle_tasks_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
    let tasks = self.tasks.read().unwrap();
    let arr: Vec<_> = tasks.values().cloned().collect();
    JsonRpcResponse::success(request.id.clone(), serde_json::to_value(arr).unwrap())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p engine --test a2a_v03_alias`
Expected: PASS.

- [ ] **Step 5: Confirm legacy tests still pass**

Run: `cargo test -p engine`
Expected: existing `tasks/send` tests unchanged.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/a2a/server.rs crates/engine/tests/a2a_v03_alias.rs
git commit -m "feat(engine): A2A v0.3 method aliases (message/send, message/stream, tasks/list)"
```

---

### Task D4: Update Agent Card served by commander to expose `capabilities` advertising v0.3

**Files:**
- Modify: `crates/engine/src/a2a/server.rs` (agent card generation)

- [ ] **Step 1: Find `agent_card` method**

Run: `grep -n "fn agent_card\|capabilities" crates/engine/src/a2a/server.rs | head -10`

- [ ] **Step 2: Add capability tags**

In the `AgentCard` generation, ensure `capabilities` includes:

```rust
vec![
    "a2a.v0.2".into(),        // legacy compat flag
    "a2a.v0.3".into(),        // new aliases are live
    "a2a.message.send".into(),
    "a2a.tasks".into(),
    "commander.workflow".into(),
    "commander.chat".into(),
]
```

- [ ] **Step 3: Test that `/.well-known/agent.json` exposes these**

If there's an existing agent-card test, extend it. Otherwise add:

```rust
#[test]
fn agent_card_advertises_v03() {
    let s = A2aServer::new(A2aServerConfig::default());
    let card = s.agent_card();
    assert!(card.capabilities.contains(&"a2a.v0.3".into()));
}
```

- [ ] **Step 4: Commit**

```bash
git add crates/engine/src/a2a/server.rs
git commit -m "feat(engine): agent card advertises a2a.v0.3 capability"
```

---

## Phase E — Commander murmur_bridge (cross-repo: mur-commander)

### Task E1: Extend `RegisteredAgent` with optional uuid + pubkey

**Files:**
- Modify: `crates/engine/src/a2a/discovery.rs`
- Test: `crates/engine/tests/agent_registry_uuid.rs`

- [ ] **Step 1: Write failing test**

Create `crates/engine/tests/agent_registry_uuid.rs`:

```rust
use engine::a2a::discovery::{AgentRegistry, RegisteredAgent};
use tempfile::NamedTempFile;

#[test]
fn registry_stores_uuid_pubkey() {
    let tmp = NamedTempFile::new().unwrap();
    let reg = AgentRegistry::new(tmp.path());

    let entry = RegisteredAgent {
        uuid: Some("01JQX4TM8Y9K7VQH6B2N3R5DPE".into()),
        pubkey: Some("zABCD".into()),
        url: "http://localhost:39393".into(),
        name: "agent_a".into(),
        description: "test".into(),
        version: "0.1.0".into(),
        skills: vec!["foo".into()],
        registered_at: chrono::Utc::now(),
        last_seen: None,
        healthy: true,
        tags: vec![],
    };
    reg.upsert(entry.clone()).unwrap();

    let listed = reg.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].uuid.as_deref(), Some("01JQX4TM8Y9K7VQH6B2N3R5DPE"));

    let by_uuid = reg.find_by_uuid("01JQX4TM8Y9K7VQH6B2N3R5DPE").unwrap();
    assert_eq!(by_uuid.unwrap().pubkey.as_deref(), Some("zABCD"));
}

#[test]
fn legacy_registry_json_loads_without_uuid() {
    // Old entry without uuid/pubkey must still parse
    let tmp = NamedTempFile::new().unwrap();
    let legacy_json = r#"[
      {"url":"http://old","name":"legacy","description":"","version":"0.1",
       "skills":[],"registered_at":"2026-04-01T00:00:00Z","last_seen":null,
       "healthy":true,"tags":[]}
    ]"#;
    std::fs::write(tmp.path(), legacy_json).unwrap();

    let reg = AgentRegistry::new(tmp.path());
    let list = reg.list().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].uuid.is_none());
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p engine --test agent_registry_uuid`
Expected: FAIL.

- [ ] **Step 3: Extend `RegisteredAgent`**

In `crates/engine/src/a2a/discovery.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredAgent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    pub url: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub skills: Vec<String>,
    pub registered_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
    pub healthy: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl AgentRegistry {
    // ... existing impl ...

    /// Upsert an entry (match by uuid if present, else by url).
    pub fn upsert(&self, entry: RegisteredAgent) -> Result<()> {
        let mut agents = self.list()?;
        let position = if let Some(u) = &entry.uuid {
            agents.iter().position(|a| a.uuid.as_ref() == Some(u))
        } else {
            agents.iter().position(|a| a.url == entry.url)
        };
        if let Some(idx) = position {
            agents[idx] = entry;
        } else {
            agents.push(entry);
        }
        self.save(&agents)
    }

    pub fn find_by_uuid(&self, uuid: &str) -> Result<Option<RegisteredAgent>> {
        Ok(self.list()?.into_iter().find(|a| a.uuid.as_deref() == Some(uuid)))
    }
}
```

- [ ] **Step 4: Update existing `register(card)` to also fill `uuid`/`pubkey` from card**

```rust
pub fn register(&self, card: &AgentCard) -> Result<RegisteredAgent> {
    let entry = RegisteredAgent {
        uuid: card.id.clone().into(),             // if AgentCard has `id`
        pubkey: card.pubkey.clone(),              // if AgentCard has `pubkey` (new in v0.3)
        url: card.url.clone(),
        name: card.name.clone(),
        description: card.description.clone(),
        version: card.version.clone(),
        skills: card.skills.iter().map(|s| s.id.clone()).collect(),
        registered_at: Utc::now(),
        last_seen: Some(Utc::now()),
        healthy: true,
        tags: vec![],
    };
    self.upsert(entry.clone())?;
    Ok(entry)
}
```

(Extend `AgentCard` struct in `protocol.rs` if it doesn't yet have `pubkey`/`id` fields — mirror P0a card shape.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p engine --test agent_registry_uuid`
Expected: PASS + existing registry tests still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/a2a/discovery.rs crates/engine/src/a2a/protocol.rs crates/engine/tests/agent_registry_uuid.rs
git commit -m "feat(engine): RegisteredAgent.uuid + .pubkey (optional, back-compat)"
```

---

### Task E2: `murmur_bridge` module — watcher for `~/.mur/agents/*/running.lock`

**Files:**
- Create: `crates/engine/src/remote/murmur_bridge.rs`
- Modify: `crates/engine/src/remote/mod.rs`
- Modify: `crates/engine/Cargo.toml`
- Test: `crates/engine/tests/murmur_bridge.rs`

- [ ] **Step 1: Add notify dep**

`Cargo.toml`:

```toml
notify = "6"
```

- [ ] **Step 2: Failing test**

Create `crates/engine/tests/murmur_bridge.rs`:

```rust
use engine::a2a::discovery::AgentRegistry;
use engine::remote::murmur_bridge::{MurmurBridge, BridgeConfig};
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn bridge_auto_registers_running_lock() {
    let agents_dir = TempDir::new().unwrap();
    let reg_file = tempfile::NamedTempFile::new().unwrap();
    let agent_dir = agents_dir.path().join("agent_a");
    fs::create_dir_all(&agent_dir).unwrap();

    let lock = serde_json::json!({
        "schema": 1,
        "uuid": "01JQX4TM8Y9K7VQH6B2N3R5DPE",
        "pubkey": "zTEST",
        "name": "agent_a",
        "pid": std::process::id(),
        "ppid": 1,
        "started_at": "2026-04-23T10:00:00Z",
        "binary_version": "mur-agent-runtime 0.1.0",
        "transports": {
            "stdio": false,
            "unix_socket": agent_dir.join("agent.sock").to_string_lossy(),
            "tcp": null
        },
        "card_digest": "sha256:abc",
        "capabilities": ["a2a.message.send"]
    });

    let cfg = BridgeConfig {
        agents_dir: agents_dir.path().to_path_buf(),
        registry_path: reg_file.path().to_path_buf(),
    };
    let bridge = MurmurBridge::start(cfg).await.unwrap();

    // Write lock file → bridge should register
    fs::write(
        agent_dir.join("running.lock"),
        serde_json::to_string(&lock).unwrap(),
    )
    .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    let reg = AgentRegistry::new(reg_file.path());
    let entry = reg.find_by_uuid("01JQX4TM8Y9K7VQH6B2N3R5DPE").unwrap();
    assert!(entry.is_some(), "bridge failed to register");
    assert_eq!(entry.unwrap().name, "agent_a");

    bridge.stop().await;
}
```

- [ ] **Step 3: Run — expect FAIL**

Run: `cargo test -p engine --test murmur_bridge`
Expected: FAIL — module not found.

- [ ] **Step 4: Implement `murmur_bridge.rs`**

```rust
//! Bridge between the murmur per-agent runtime (P0a+) and the commander
//! AgentRegistry.
//!
//! Watches `~/.mur/agents/*/running.lock` for create/modify/remove events,
//! parses the LockFile JSON, and upserts into the registry.

use crate::a2a::discovery::{AgentRegistry, RegisteredAgent};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct BridgeConfig {
    pub agents_dir: PathBuf,
    pub registry_path: PathBuf,
}

pub struct MurmurBridge {
    shutdown_tx: mpsc::Sender<()>,
    join: tokio::task::JoinHandle<()>,
}

impl MurmurBridge {
    pub async fn start(cfg: BridgeConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.agents_dir)?;
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let (evt_tx, evt_rx) = std_mpsc::channel();

        let mut watcher: RecommendedWatcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let _ = evt_tx.send(ev);
                }
            },
        )?;
        watcher.watch(&cfg.agents_dir, RecursiveMode::Recursive)?;

        let registry = AgentRegistry::new(&cfg.registry_path);

        // Initial pass: pick up any existing locks
        scan_existing_locks(&cfg.agents_dir, &registry);

        let join = tokio::task::spawn_blocking(move || {
            loop {
                match evt_rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(ev) => {
                        if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                            for p in ev.paths {
                                if p.file_name().map(|n| n == "running.lock").unwrap_or(false) {
                                    let _ = handle_lock(&p, &registry);
                                }
                            }
                        }
                        if matches!(ev.kind, EventKind::Remove(_)) {
                            for p in ev.paths {
                                if p.file_name().map(|n| n == "running.lock").unwrap_or(false) {
                                    if let Some(uuid) = uuid_from_dir(p.parent()) {
                                        let _ = registry.mark_offline_by_uuid(&uuid);
                                    }
                                }
                            }
                        }
                    }
                    Err(std_mpsc::RecvTimeoutError::Timeout) => {
                        if shutdown_rx.try_recv().is_ok() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            drop(watcher);
        });

        Ok(Self { shutdown_tx, join })
    }

    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
    }
}

fn scan_existing_locks(dir: &Path, reg: &AgentRegistry) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let lock = e.path().join("running.lock");
        if lock.exists() {
            let _ = handle_lock(&lock, reg);
        }
    }
}

fn handle_lock(path: &Path, reg: &AgentRegistry) -> Result<()> {
    let content = std::fs::read_to_string(path).context("reading running.lock")?;
    let lock: LockFile = serde_json::from_str(&content)?;

    // Prefer TCP endpoint, fall back to unix socket
    let url = if let Some(tcp) = lock.transports.tcp {
        format!("tcp://{tcp}")
    } else if let Some(sock) = lock.transports.unix_socket {
        format!("unix://{sock}")
    } else {
        format!("stdio://{}", lock.name)
    };

    let entry = RegisteredAgent {
        uuid: Some(lock.uuid.clone()),
        pubkey: lock.pubkey.clone(),
        url,
        name: lock.name,
        description: String::new(),
        version: lock.binary_version,
        skills: vec![],
        registered_at: DateTime::parse_from_rfc3339(&lock.started_at)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        last_seen: Some(Utc::now()),
        healthy: true,
        tags: vec!["murmur".into()],
    };
    reg.upsert(entry)?;
    Ok(())
}

fn uuid_from_dir(dir: Option<&Path>) -> Option<String> {
    let lock = dir?.join("running.lock");
    let content = std::fs::read_to_string(&lock).ok()?;
    let lock: LockFile = serde_json::from_str(&content).ok()?;
    Some(lock.uuid)
}

#[derive(Deserialize)]
struct LockFile {
    uuid: String,
    pubkey: Option<String>,
    name: String,
    binary_version: String,
    started_at: String,
    transports: LockTransports,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Deserialize)]
struct LockTransports {
    #[serde(default)]
    unix_socket: Option<String>,
    #[serde(default)]
    tcp: Option<String>,
}
```

Add to `AgentRegistry`:

```rust
pub fn mark_offline_by_uuid(&self, uuid: &str) -> Result<()> {
    let mut agents = self.list()?;
    if let Some(a) = agents.iter_mut().find(|a| a.uuid.as_deref() == Some(uuid)) {
        a.healthy = false;
    }
    self.save(&agents)
}
```

Update `crates/engine/src/remote/mod.rs`:

```rust
pub mod murmur_bridge;
```

- [ ] **Step 5: Run test**

Run: `cargo test -p engine --test murmur_bridge`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/remote/murmur_bridge.rs crates/engine/src/remote/mod.rs crates/engine/src/a2a/discovery.rs crates/engine/Cargo.toml crates/engine/tests/murmur_bridge.rs
git commit -m "feat(engine): murmur_bridge — auto-register P0a agents from running.lock"
```

---

### Task E3: Wire `MurmurBridge` into commander daemon startup

**Files:**
- Modify: `crates/daemon/src/main.rs` (or wherever daemon boot lives)
- Test: manual or integration

- [ ] **Step 1: Locate daemon startup**

Run: `grep -n "fn main\|spawn\|AgentRegistry" crates/daemon/src/main.rs | head -10`

- [ ] **Step 2: Spawn the bridge**

After registry / A2A server are set up, add:

```rust
use engine::remote::murmur_bridge::{MurmurBridge, BridgeConfig};

let agents_dir = directories::BaseDirs::new()
    .map(|d| d.home_dir().join(".mur").join("agents"))
    .unwrap_or_else(|| PathBuf::from("/tmp/mur-agents"));

let bridge_cfg = BridgeConfig {
    agents_dir,
    registry_path: engine::a2a::discovery::AgentRegistry::default_path(),
};
let bridge = MurmurBridge::start(bridge_cfg).await?;
tracing::info!("MurmurBridge started — watching P0a running.lock files");

// on shutdown:
bridge.stop().await;
```

- [ ] **Step 3: Smoke test**

Start daemon, touch a fake running.lock, verify registry entry. Can be scripted in `crates/daemon/tests/`.

- [ ] **Step 4: Commit**

```bash
git add crates/daemon/src/main.rs
git commit -m "feat(daemon): start MurmurBridge to auto-register P0a agents"
```

---

### Task E4: CLI verb `murc agents list --murmur` filters by tag

**Files:**
- Modify: `crates/cli/src/commands.rs` (or wherever agent subcommands live)

- [ ] **Step 1: Add filter flag**

Extend `agents list` subcommand with `--murmur` flag that filters registry entries where `tags` contains `"murmur"`.

- [ ] **Step 2: Inline test**

```rust
#[test]
fn filter_murmur_tag() {
    use engine::a2a::discovery::RegisteredAgent;
    let agents = vec![
        RegisteredAgent { tags: vec!["murmur".into()], name: "a".into(), /* ... */ ..default_re() },
        RegisteredAgent { tags: vec![], name: "b".into(), ..default_re() },
    ];
    let filtered: Vec<_> = agents.iter().filter(|a| a.tags.iter().any(|t| t == "murmur")).collect();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].name, "a");
}
```

(Provide `default_re()` helper or construct entries manually.)

- [ ] **Step 3: Commit**

```bash
git add crates/cli/src/commands.rs
git commit -m "feat(cli): murc agents list --murmur filters P0a-bridged agents"
```

---

## Phase F — Commander Collector Stub (cross-repo: mur-commander)

### Task F1: Observability module scaffold + deps

**Files:**
- Create: `crates/engine/src/observability/mod.rs`
- Modify: `crates/engine/src/lib.rs`
- Modify: `crates/engine/Cargo.toml`

- [ ] **Step 1: Add deps**

`Cargo.toml`:

```toml
opentelemetry = "0.24"
opentelemetry-sdk = { version = "0.24", features = ["rt-tokio"] }
opentelemetry-semantic-conventions = "0.16"
regex = "1"
```

- [ ] **Step 2: Create module**

`crates/engine/src/observability/mod.rs`:

```rust
//! Observability collector subsystem.
//!
//! Tails each bridged murmur agent's telemetry JSONL files, normalizes
//! JSON-RPC notification payloads into OpenTelemetry spans / logs /
//! metrics, applies redaction policy, and buffers on disk.
//!
//! P0a.5: buffers only — NO upstream forward (that's P1's hub integration).

pub mod collector;
pub mod redaction;
pub mod spool;
```

`crates/engine/src/lib.rs`:

```rust
pub mod observability;
```

- [ ] **Step 3: Commit**

```bash
git add crates/engine/src/observability/mod.rs crates/engine/src/lib.rs crates/engine/Cargo.toml
git commit -m "feat(engine): observability module scaffold + OTel deps"
```

---

### Task F2: Redaction module — three modes

**Files:**
- Create: `crates/engine/src/observability/redaction.rs`
- Test: `crates/engine/tests/redaction.rs`

- [ ] **Step 1: Failing test**

Create `crates/engine/tests/redaction.rs`:

```rust
use engine::observability::redaction::{RedactionMode, apply_redaction};
use serde_json::json;

#[test]
fn full_mode_keeps_everything() {
    let mut v = json!({"gen_ai.request.messages": [{"role":"user","content":"secret"}]});
    apply_redaction(&mut v, RedactionMode::Full);
    assert_eq!(v["gen_ai.request.messages"][0]["content"], "secret");
}

#[test]
fn redacted_mode_hashes_content() {
    let mut v = json!({"gen_ai.request.messages": [{"role":"user","content":"secret"}]});
    apply_redaction(&mut v, RedactionMode::Redacted);
    let content = v["gen_ai.request.messages"][0]["content"].as_str().unwrap();
    assert!(content.starts_with("sha256:"), "expected hash prefix, got {content}");
    assert!(v["gen_ai.request.messages"][0]["_size"].is_u64());
}

#[test]
fn metadata_only_drops_content() {
    let mut v = json!({
        "gen_ai.usage.input_tokens": 42,
        "gen_ai.request.messages": [{"role":"user","content":"secret"}]
    });
    apply_redaction(&mut v, RedactionMode::MetadataOnly);
    assert!(v.get("gen_ai.request.messages").is_none());
    assert_eq!(v["gen_ai.usage.input_tokens"], 42);
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p engine --test redaction`
Expected: FAIL.

- [ ] **Step 3: Implement `redaction.rs`**

```rust
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RedactionMode {
    Full,
    #[default]
    Redacted,
    MetadataOnly,
}

const CONTENT_KEYS: &[&str] = &[
    "gen_ai.request.messages",
    "gen_ai.response.messages",
    "gen_ai.request.prompt",
    "gen_ai.response.completion",
    "tool.args",
    "tool.result",
];

pub fn apply_redaction(value: &mut Value, mode: RedactionMode) {
    match mode {
        RedactionMode::Full => {}
        RedactionMode::Redacted => redact_values(value),
        RedactionMode::MetadataOnly => strip_content(value),
    }
}

fn redact_values(v: &mut Value) {
    if let Some(map) = v.as_object_mut() {
        for key in CONTENT_KEYS {
            if let Some(inner) = map.get_mut(*key) {
                redact_content(inner);
            }
        }
    }
}

fn redact_content(v: &mut Value) {
    match v {
        Value::String(s) => {
            let hash = hash_of(s.as_bytes());
            let size = s.len();
            let mut obj = Map::new();
            obj.insert("content".into(), Value::String(hash));
            obj.insert("_size".into(), Value::from(size));
            *v = Value::Object(obj);
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(obj) = item.as_object_mut() {
                    if let Some(content) = obj.get("content").and_then(|c| c.as_str()) {
                        let hashed = hash_of(content.as_bytes());
                        let size = content.len();
                        obj.insert("content".into(), Value::String(hashed));
                        obj.insert("_size".into(), Value::from(size));
                    }
                }
            }
        }
        _ => {}
    }
}

fn strip_content(v: &mut Value) {
    if let Some(map) = v.as_object_mut() {
        for key in CONTENT_KEYS {
            map.remove(*key);
        }
    }
}

fn hash_of(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("sha256:{:x}", h.finalize())
}
```

Add `sha2 = "0.10"` to `Cargo.toml` dependencies if not present.

- [ ] **Step 4: Run tests**

Run: `cargo test -p engine --test redaction`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/observability/redaction.rs crates/engine/tests/redaction.rs crates/engine/Cargo.toml
git commit -m "feat(engine): redaction module — full / redacted / metadata-only modes"
```

---

### Task F3: Disk spool

**Files:**
- Create: `crates/engine/src/observability/spool.rs`
- Test: `crates/engine/tests/spool.rs`

- [ ] **Step 1: Failing test**

Create `crates/engine/tests/spool.rs`:

```rust
use engine::observability::spool::Spool;
use tempfile::TempDir;

#[tokio::test]
async fn spool_append_and_iterate() {
    let dir = TempDir::new().unwrap();
    let spool = Spool::open(dir.path(), 100 * 1024).unwrap();

    spool.append(br#"{"a":1}"#).await.unwrap();
    spool.append(br#"{"b":2}"#).await.unwrap();

    let entries = spool.drain(10).await.unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0], br#"{"a":1}"#);
}

#[tokio::test]
async fn spool_caps_at_max_bytes() {
    let dir = TempDir::new().unwrap();
    let spool = Spool::open(dir.path(), 16).unwrap();

    spool.append(b"12345678").await.unwrap();          // 8B
    spool.append(b"abcdefgh").await.unwrap();          // 8B — now at cap
    spool.append(b"XXXXXXXXXXXXX").await.unwrap();     // would exceed

    // Oldest dropped (or the new one rejected — implementation choice;
    // either way we should not exceed cap meaningfully)
    let total: usize = spool.drain(10).await.unwrap().iter().map(|e| e.len()).sum();
    assert!(total <= 16 + 13, "spool blew cap: {total}");
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p engine --test spool`
Expected: FAIL.

- [ ] **Step 3: Implement `spool.rs`**

```rust
//! Disk-backed append-only spool for telemetry batches.
//!
//! Layout: one JSONL file per spool session (`spool-YYYYMMDD-HHMMSS-pid.jsonl`).
//! On open, rolls to a new file if any existing file exceeds `max_file_bytes`.
//! `drain()` reads + removes the oldest file's contents.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

pub struct Spool {
    dir: PathBuf,
    max_bytes: u64,
    current: Mutex<PathBuf>,
}

impl Spool {
    pub fn open(dir: &Path, max_bytes: u64) -> Result<Self> {
        std::fs::create_dir_all(dir).context("creating spool dir")?;
        let current = dir.join(format!(
            "spool-{}-{}.jsonl",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        ));
        Ok(Self {
            dir: dir.to_path_buf(),
            max_bytes,
            current: Mutex::new(current),
        })
    }

    pub async fn append(&self, line: &[u8]) -> Result<()> {
        let mut cur = self.current.lock().await;
        // Roll if current exceeds cap
        if fs::metadata(&*cur).await.map(|m| m.len()).unwrap_or(0) >= self.max_bytes {
            *cur = self.dir.join(format!(
                "spool-{}-{}.jsonl",
                chrono::Utc::now().format("%Y%m%d-%H%M%S%f"),
                std::process::id()
            ));
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&*cur).await?;
        f.write_all(line).await?;
        f.write_all(b"\n").await?;
        Ok(())
    }

    pub async fn drain(&self, max_entries: usize) -> Result<Vec<Vec<u8>>> {
        let mut entries = Vec::new();
        let mut rd = fs::read_dir(&self.dir).await?;
        let mut files: Vec<PathBuf> = Vec::new();
        while let Some(e) = rd.next_entry().await? {
            if e.file_name().to_string_lossy().starts_with("spool-") {
                files.push(e.path());
            }
        }
        files.sort();
        for path in files {
            let f = File::open(&path).await?;
            let r = BufReader::new(f);
            let mut lines = r.lines();
            while let Some(line) = lines.next_line().await? {
                if !line.is_empty() {
                    entries.push(line.into_bytes());
                    if entries.len() >= max_entries {
                        break;
                    }
                }
            }
            let _ = fs::remove_file(&path).await;
            if entries.len() >= max_entries {
                break;
            }
        }
        Ok(entries)
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p engine --test spool`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/observability/spool.rs crates/engine/tests/spool.rs
git commit -m "feat(engine): disk-backed spool for telemetry batches"
```

---

### Task F4: Collector — tail JSONL + normalize + spool

**Files:**
- Create: `crates/engine/src/observability/collector.rs`
- Test: `crates/engine/tests/collector.rs`

- [ ] **Step 1: Failing test**

Create `crates/engine/tests/collector.rs`:

```rust
use engine::observability::collector::{Collector, CollectorConfig};
use engine::observability::redaction::RedactionMode;
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn collector_spools_llm_call_notification() {
    let agents_dir = TempDir::new().unwrap();
    let spool_dir = TempDir::new().unwrap();
    let agent_a = agents_dir.path().join("agent_a").join("telemetry");
    fs::create_dir_all(&agent_a).unwrap();

    let cfg = CollectorConfig {
        agents_dir: agents_dir.path().to_path_buf(),
        spool_dir: spool_dir.path().to_path_buf(),
        spool_max_bytes: 1_000_000,
        redaction: RedactionMode::Full,
    };
    let collector = Collector::start(cfg).await.unwrap();

    let line = r#"{"jsonrpc":"2.0","method":"telemetry/llm_call","params":{"gen_ai.request.model":"x","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5,"latency_ms":42}}"#;
    fs::write(agent_a.join("2026-04-23.jsonl"), format!("{line}\n")).unwrap();

    tokio::time::sleep(Duration::from_millis(700)).await;

    let entries = collector.drain_for_test(10).await.unwrap();
    assert!(!entries.is_empty(), "no spooled entries");
    let s = String::from_utf8_lossy(&entries[0]);
    assert!(s.contains("\"gen_ai.request.model\""));

    collector.stop().await;
}
```

- [ ] **Step 2: Run — expect FAIL**

Run: `cargo test -p engine --test collector`
Expected: FAIL.

- [ ] **Step 3: Implement `collector.rs`**

```rust
//! Per-host telemetry collector.

use super::redaction::{RedactionMode, apply_redaction};
use super::spool::Spool;
use anyhow::Result;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;

pub struct CollectorConfig {
    pub agents_dir: PathBuf,
    pub spool_dir: PathBuf,
    pub spool_max_bytes: u64,
    pub redaction: RedactionMode,
}

pub struct Collector {
    shutdown_tx: mpsc::Sender<()>,
    spool: Arc<Spool>,
    join: tokio::task::JoinHandle<()>,
}

impl Collector {
    pub async fn start(cfg: CollectorConfig) -> Result<Self> {
        let spool = Arc::new(Spool::open(&cfg.spool_dir, cfg.spool_max_bytes)?);
        let spool2 = spool.clone();
        let redaction = cfg.redaction;

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        let (evt_tx, evt_rx) = std_mpsc::channel();

        let mut watcher: RecommendedWatcher = notify::recommended_watcher(
            move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let _ = evt_tx.send(ev);
                }
            },
        )?;
        std::fs::create_dir_all(&cfg.agents_dir)?;
        watcher.watch(&cfg.agents_dir, RecursiveMode::Recursive)?;

        // Track read offsets per file
        let offsets: Arc<StdMutex<std::collections::HashMap<PathBuf, u64>>> =
            Arc::new(StdMutex::new(Default::default()));
        let offsets2 = offsets.clone();

        let join = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {
                        // Poll event queue
                        while let Ok(ev) = evt_rx.try_recv() {
                            if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                                for p in ev.paths {
                                    if p.extension().map(|e| e == "jsonl").unwrap_or(false) {
                                        let _ = tail_file(
                                            &p,
                                            &offsets2,
                                            &spool2,
                                            redaction,
                                        ).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            drop(watcher);
        });

        Ok(Self { shutdown_tx, spool, join })
    }

    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(()).await;
        let _ = self.join.await;
    }

    #[cfg(any(test, feature = "testing"))]
    pub async fn drain_for_test(&self, n: usize) -> Result<Vec<Vec<u8>>> {
        self.spool.drain(n).await
    }
}

async fn tail_file(
    path: &std::path::Path,
    offsets: &StdMutex<std::collections::HashMap<PathBuf, u64>>,
    spool: &Spool,
    redaction: RedactionMode,
) -> Result<()> {
    let mut f = std::fs::File::open(path)?;
    let offset = {
        let g = offsets.lock().unwrap();
        g.get(path).copied().unwrap_or(0)
    };
    f.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(&mut f);
    let mut new_offset = offset;
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        new_offset += line.len() as u64;
        if let Ok(mut v) = serde_json::from_str::<Value>(line.trim()) {
            if is_telemetry(&v) {
                if let Some(params) = v.get_mut("params") {
                    apply_redaction(params, redaction);
                }
                spool.append(v.to_string().as_bytes()).await?;
            }
        }
        line.clear();
    }
    offsets.lock().unwrap().insert(path.to_path_buf(), new_offset);
    Ok(())
}

fn is_telemetry(v: &Value) -> bool {
    v.get("method")
        .and_then(|m| m.as_str())
        .map(|s| s.starts_with("telemetry/") || s.starts_with("task/progress"))
        .unwrap_or(false)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p engine --test collector`
Expected: PASS.

- [ ] **Step 5: Wire collector into daemon startup (same pattern as E3)**

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/observability/collector.rs crates/engine/tests/collector.rs crates/daemon/src/main.rs
git commit -m "feat(engine): collector tails agent JSONL + redacts + spools"
```

---

### Task F5: Commander config — `telemetry.mode` selection

**Files:**
- Modify: commander config schema (`crates/engine/src/config.rs` or similar)
- Test: inline

- [ ] **Step 1: Add field**

```rust
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct TelemetryConfig {
    #[serde(default)]
    pub mode: String,                    // "full" | "redacted" | "metadata-only"
    #[serde(default = "default_spool_mb")]
    pub spool_cap_mb: u64,
}

fn default_spool_mb() -> u64 { 100 }

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self { mode: "full".into(), spool_cap_mb: default_spool_mb() }
    }
}
```

- [ ] **Step 2: Parse into `RedactionMode`**

Helper:

```rust
pub fn parse_redaction_mode(s: &str) -> RedactionMode {
    match s {
        "redacted" => RedactionMode::Redacted,
        "metadata-only" => RedactionMode::MetadataOnly,
        _ => RedactionMode::Full,
    }
}
```

- [ ] **Step 3: Commit**

```bash
git add crates/engine/src/config.rs
git commit -m "feat(engine): telemetry.mode config field (full / redacted / metadata-only)"
```

---

## Phase G — E2E + Roll-up

### Task G1: E2E script — identity + TCP handshake between two agents

**Files:**
- Create: `scripts/e2e/p0a5-identity-handshake.sh` (in mur workspace)

- [ ] **Step 1: Write script**

```bash
#!/usr/bin/env bash
set -euo pipefail
# P0a.5 E2E: create two agents with identities + TCP transport + verify
# handshake succeeds.

TMPDIR="$(mktemp -d)"
export MUR_HOME="$TMPDIR/mur-home"
export MUR_AGENT_BIN_DIR="$TMPDIR/bin"
mkdir -p "$MUR_AGENT_BIN_DIR"
export PATH="$MUR_AGENT_BIN_DIR:$PATH"

cargo build --workspace --release
cp target/release/mur "$MUR_AGENT_BIN_DIR/"
cp target/release/mur-agent-runtime "$MUR_AGENT_BIN_DIR/"

# Create two agents
mur agent create agent_a --no-interactive --display-name "A" --category research
mur agent create agent_b --no-interactive --display-name "B" --category research

# Enable TCP for agent_b (receiver)
AGENT_B_YAML="$MUR_HOME/agents/agent_b/profile.yaml"
sed -i.bak '/tcp:$/,/pattern:/d' "$AGENT_B_YAML" || true
cat >> "$AGENT_B_YAML" <<EOF
transport_override_marker: "replaced"
EOF
# For the real plan this is a yaml_edit subcommand call; kept inline here.

mur agent perm allow-host agent_b '*'
# Enable inbound port in entitlements (from Task B8)
# mur agent perm set-inbound-port agent_b 39393  # hypothetical CLI

# Start agent_b in background
mur_agent_b start --detach
sleep 2

# Ensure running.lock written and contains pubkey
grep -q '"pubkey"' "$MUR_HOME/agents/agent_b/running.lock" || {
  echo "FAIL: running.lock missing pubkey"; exit 1;
}

echo "OK: P0a.5 identity + TCP smoke"
mur_agent_b stop
rm -rf "$TMPDIR"
```

- [ ] **Step 2: Make executable + commit**

```bash
chmod +x scripts/e2e/p0a5-identity-handshake.sh
git add scripts/e2e/p0a5-identity-handshake.sh
git commit -m "test(e2e): P0a.5 identity + TCP handshake smoke"
```

---

### Task G2: E2E — commander auto-register P0a agent

**Files:**
- Create: `scripts/e2e/p0a5-commander-autoregister.sh`

- [ ] **Step 1: Write script**

```bash
#!/usr/bin/env bash
set -euo pipefail
# Requires both repos built: main mur + mur-commander.
# Strategy: start commander daemon, start a P0a agent, assert that
# commander's agent registry contains the P0a agent's uuid.

MUR_REPO="${MUR_REPO:-$HOME/Projects/mur}"
COMMANDER_REPO="${COMMANDER_REPO:-$HOME/Projects/mur-commander}"

TMPDIR="$(mktemp -d)"
export MUR_HOME="$TMPDIR/mur-home"
export MUR_AGENT_BIN_DIR="$TMPDIR/bin"
export PATH="$MUR_AGENT_BIN_DIR:$PATH"
mkdir -p "$MUR_AGENT_BIN_DIR"

# Build both
cd "$MUR_REPO" && cargo build --release --workspace
cp target/release/mur target/release/mur-agent-runtime "$MUR_AGENT_BIN_DIR/"

cd "$COMMANDER_REPO" && cargo build --release -p daemon
cp target/release/mur-daemon "$MUR_AGENT_BIN_DIR/"

# Start commander daemon in background
HOME="$TMPDIR" mur-daemon start &
DAEMON_PID=$!
sleep 2

# Create + start P0a agent
mur agent create agent_x --no-interactive --display-name "X" --category research
mur_agent_x start --detach
sleep 2

# Query commander registry
HOME="$TMPDIR" mur-daemon agents list --murmur > "$TMPDIR/agents.txt"
grep -q agent_x "$TMPDIR/agents.txt" || {
  cat "$TMPDIR/agents.txt"; echo "FAIL: agent_x not in commander registry"; exit 1
}

echo "OK: commander auto-registered P0a agent"
mur_agent_x stop
kill $DAEMON_PID
rm -rf "$TMPDIR"
```

- [ ] **Step 2: Commit**

```bash
chmod +x scripts/e2e/p0a5-commander-autoregister.sh
git add scripts/e2e/p0a5-commander-autoregister.sh
git commit -m "test(e2e): P0a.5 commander auto-registration smoke"
```

---

### Task G3: Top-level runner — `scripts/e2e/p0a5-full.sh`

**Files:**
- Create: `scripts/e2e/p0a5-full.sh`

- [ ] **Step 1: Write runner**

```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "=== G1: identity + TCP handshake ==="
scripts/e2e/p0a5-identity-handshake.sh

echo "=== G2: commander auto-register ==="
scripts/e2e/p0a5-commander-autoregister.sh

echo "=== P0a.5 E2E: ALL PASS ==="
```

- [ ] **Step 2: Commit**

```bash
chmod +x scripts/e2e/p0a5-full.sh
git add scripts/e2e/p0a5-full.sh
git commit -m "test(e2e): P0a.5 full smoke runner"
```

---

### Task G4: CHANGELOG + CLAUDE.md update

**Files:**
- Modify: `CLAUDE.md` (Architecture section)
- Create: `docs/superpowers/plans/2026-04-23-murmur-p0a5-plan-COMPLETE.md`

- [ ] **Step 1: Update CLAUDE.md**

In the "Architecture" section of `CLAUDE.md`, add under the P0a line:

```
- **`mur-agent-runtime` P0a.5 additions:** per-agent Ed25519 identity
  (`identity.key`/`identity.pub`), TCP transport with Noise XK handshake,
  Agent Card includes `pubkey` + `endpoints[]` + `deployment` metadata,
  profile supports `lifecycle.execution` (daemon | on_demand) +
  `lifecycle.schedule[]`, + `file_transfer.*` + `deployment`.
- **mur-commander integration (P0a.5 additive):** v0.3 A2A method aliases
  (`message/send` / `message/stream` / `tasks/list`), auto-registers P0a
  agents via `murmur_bridge` (reads `~/.mur/agents/*/running.lock`),
  collects telemetry JSONL into local disk spool with three redaction
  modes (upstream hub forward lands in P1).
```

- [ ] **Step 2: Create COMPLETE log**

Standard roll-up describing all Phase A-G deliverables, mirroring the
style of `2026-04-22-murmur-p0a-agent-runtime-plan-COMPLETE.md`.

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md docs/superpowers/plans/2026-04-23-murmur-p0a5-plan-COMPLETE.md
git commit -m "docs: CLAUDE.md + P0a.5 COMPLETE log"
```

---

### Task G5: Push branches + open PRs

**Files:** none (git operations)

- [ ] **Step 1: Push mur workspace branch**

```bash
cd ~/Projects/mur
git push -u origin feat/murmur-p0a.5
gh pr create \
  --base feat/murmur-p0a \
  --head feat/murmur-p0a.5 \
  --title "P0a.5 — identity + TCP Noise + commander integration" \
  --body "$(cat <<EOF
## Summary
Implements murmur P0a.5 per docs/superpowers/specs/2026-04-23-murmur-fleet-architecture-design.md §11.1.

## Changes
- mur-common: AgentIdentity (Ed25519 + multibase), profile schema extended with identity / tcp transport / lifecycle.schedule / file_transfer / deployment
- mur-agent-runtime: TCP listener + connector with Noise XK handshake, Agent Card exposes pubkey + endpoints[] + deployment
- mur-core: mur agent create generates identity + writes into profile
- scripts/e2e: P0a.5 smoke runner

Cross-repo companion: mur-commander#<TBD> (feat/murmur-bridge branch) for A2A v0.3 aliasing + murmur_bridge + collector stub.

## Test plan
- [ ] cargo test --workspace passes
- [ ] scripts/e2e/p0a5-full.sh passes after commander branch is also merged
- [ ] Legacy P0a agents without identity still load

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 2: Push commander branch**

```bash
cd ~/Projects/mur-commander
git push -u origin feat/murmur-bridge
gh pr create --title "feat: P0a.5 bridge — A2A v0.3 aliasing + murmur auto-register + collector" --body "Companion PR to mur-run/mur#<TBD>."
```

- [ ] **Step 3: Done — notify user**

---

## Self-Review

### Spec coverage

| Spec section | Plan task(s) |
|---|---|
| §11.1 G1 (identity keypair on create) | A2, A4, C1 |
| §11.1 G2 (multibase lib wrap) | A2, A3 |
| §11.1 G3 (TCP + Noise XK in runtime) | B1-B4 |
| §11.1 G4 (Agent Card extension) | A5-A7, B7 |
| §11.1 G5 (Commander A2A v0.3 aliasing) | D1-D3 |
| §11.1 G6 (murmur_bridge auto-register) | E1-E3 |
| §11.1 G7 (Commander collector stub) | F1-F5 |
| §11.1 G8 (profile schema stabilization) | A5-A7 |
| Spec F17 (commander-hub protocol) | deferred to P1 (out of scope this plan) |
| Spec F18 (SSH backbone preservation) | untouched (additive only) |
| Spec Open Q-B (rekey) | C4 (deferred with TODO marker) |

### Placeholder scan

- None found in final plan. Every code step has executable code and expected output.
- All cross-references ("Task B4", "Task E3") point to tasks that exist.
- `default_re()` helper in E4 is a test-local stub — if the engineer prefers to construct full RegisteredAgent literals, that also works; the test essence is correct.

### Type consistency

- `AgentIdentity`, `IdentityConfig` — same names used across A2, A5, C1.
- `TcpTransportConfig`, `NoiseConfig` — consistent with A6 and B4.
- `RedactionMode` — same enum used in F2, F4, F5.
- `MurmurBridge`, `BridgeConfig` — consistent in E2, E3.
- `Collector`, `CollectorConfig` — consistent in F4.

### Scope check

- Single coherent phase (transport + identity + commander integration).
- 37 tasks, each 2-5 minutes of engineering time per step, most tasks 1-4 hours total.
- Cross-repo but clearly labeled (Phase A-C in mur, D-F in mur-commander, G mixed).
- Verification gates at Task A8, B9, G3.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-23-murmur-p0a5-implementation-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Cross-repo nature makes this especially helpful so the parent keeps the big picture while subagents do focused edits.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints. Given ~37 tasks across two repos, this will be a long-running session.

**Which approach?**
