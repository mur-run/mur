# Commander → MuR Feedback Wire Protocol — v1 Freeze

**Status**: proposed (schema freeze)
**Author**: david + Claude Opus 4.7
**Date**: 2026-05-18
**Related**:
- `plans/2026-05-18-continual-learning-versioned-evolution.md` v2 (§8 E5, D4, D8)
- `docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md` (original design)
- `docs/superpowers/plans/2026-05-18-mur-commander-channel1-closure.md` (implementation plan)
- `mur-common/src/signal.rs` (existing schema, 11 round-trip tests)
- `mur-common/src/bridge/envelope.rs` (existing Ed25519 wrapper, C7 reuse)
- `mur-core/src/sync/inbox.rs` (live receive-side)

## Problem

The continual-learning v2 spec identified **D8 as the highest-priority gap**: commander's `LongTermStore` does not write back to `~/.mur/patterns/`, so execution evidence (thousands of data points per active user) never reaches mur's Evidence/Maturity loop.

A deep code survey found that **the gap is asymmetric and smaller than the 2026-04-18 spec implied**:

| Side | Status |
|---|---|
| `mur-common::signal::Signal` (envelope) | ✅ live, schema v1, 11 round-trip tests |
| `SignalKind` (5 variants covering C1/C2/C3) | ✅ live |
| `Actor` / `ActorSource` (8 sources incl. `CommanderDaemon`) | ✅ live |
| `mur-common::bridge::envelope::SignedEnvelope` (Ed25519) | ✅ live (C7 Slack bridge already uses it) |
| `mur-core::sync::inbox::Inbox::apply_all()` | ✅ live, used by `mur fetch` |
| `mur-core::sync::outbox::Outbox` | ✅ live |
| **HTTP endpoint `POST /v1/signals/batch` on mur-daemon** | ❌ **not implemented** |
| **Bearer-token auth on signal endpoints** | ❌ **not implemented** |
| **Commander-side `LocalBridge` / outbox writer** | ❌ **not implemented** (closed-source repo) |
| **Idempotency / dedup at HTTP layer** | ❌ **not specified** |

→ The schema is essentially already frozen by code shipped 2026-04-18. **What blocks D8 closure is the HTTP contract + commander-side implementation, not a new envelope type.**

This spec freezes the contract so the commander team can implement outbox writers in parallel without ambiguity, and so mur-daemon can wire up the receiving HTTP endpoint against a stable target.

## Decisions

1. **Schema freeze: `Signal` v1 is frozen.** `mur-common::signal::Signal` and its supporting types (`SignalKind`, `SignalTarget`, `Actor`, `ActorSource`, `Scope`) are stable. Any change requires a SCHEMA_VERSION bump to 2 and a coordinated migration.
2. **HTTP endpoints are versioned in the URL path**, not in JSON. `POST /v1/signals/batch`, `GET /v1/signals/pending`. v2 endpoints would coexist at `/v2/...`.
3. **Auth: bearer token in v1**, Ed25519 envelope **optional in v1 / mandatory in v2.** Matches D4 from the continual-learning v2 spec.
4. **Idempotency by `Signal.id` (UUID).** Server keeps a dedup window (default 7 days) keyed by `signal.id`; replay returns 200 with `{ "deduplicated": true }`.
5. **Batch semantics: all-or-nothing per request.** Either every signal in the batch is accepted (or deduplicated) or the entire batch is rejected. Partial success is a footgun for at-least-once writers.
6. **HTTP server lives on mur-daemon, bound to `127.0.0.1`.** Cross-machine deferred to v2 (per D4).
7. **`SignedEnvelope` wraps the canonical-JSON Signal in v1+ optionally**, mandatory in v2. v1 servers accept both `Content-Type: application/json` (raw Signal/Batch) and `Content-Type: application/mur.signed-envelope+json` (signed wrapper) — clients pick.
8. **Commander writes outbox files in the same on-disk YAML format that mur-core's `Inbox::receive()` accepts** — so a degenerate file-drop integration (commander writes to a shared `~/.mur/inbox/` directly) works without HTTP if both binaries run on the same machine. HTTP is the recommended path; file-drop is a documented fallback.
9. **Frozen wire format is verified by a snapshot test in `mur-common`.** Any accidental schema change breaks CI before it ships.
10. **One canonical commander-side reference implementation lives in this spec (§5)**. Commander team implements against it; mur team owns the receive side.

## §1 Architecture

```
                    ┌─────────────────────────────────────┐
                    │ commander daemon (closed-source)    │
                    │                                     │
                    │  workflow exec ──▶ LocalBridge      │
                    │                       │              │
                    │                       ▼              │
                    │   ~/.mur/commander/outbox/*.yaml    │  (new directory,
                    │                       │              │   commander-owned)
                    │                       ▼              │
                    │   HTTP POST /v1/signals/batch       │
                    └───────────────┬─────────────────────┘
                                    │ Bearer token (v1)
                                    │ + optional SignedEnvelope (v1)
                                    │ + mandatory SignedEnvelope (v2)
                                    ▼
                    ┌─────────────────────────────────────┐
                    │ mur-daemon (mur-core)               │
                    │                                     │
                    │  server_agents::routes::signals     │  (NEW handler)
                    │            │                         │
                    │            ▼                         │
                    │  Inbox::receive() per signal        │  (existing)
                    │            │                         │
                    │            ▼                         │
                    │   ~/.mur/inbox/*.yaml               │  (existing)
                    │            │                         │
                    │            ▼                         │
                    │  E3 sleep cycle drain_inbox()       │
                    │            │                         │
                    │            ▼                         │
                    │  Inbox::apply_all(&store)           │  (existing)
                    │            │                         │
                    │            ▼                         │
                    │  YamlStore update + E1 commit       │
                    └─────────────────────────────────────┘
```

**File-drop fallback (same-machine, no HTTP)**: commander writes directly to `~/.mur/inbox/<ts>-<uuid>.yaml`. mur sleep cycle picks it up on next drain. Identical semantics minus auth + idempotency at the transport.

## §2 Schema Freeze

The frozen schema is **the code at `mur-common/src/signal.rs`** as of this date. No prose redefinition here (would drift). Reproduced for ergonomics only:

```rust
pub const SIGNAL_SCHEMA_VERSION: u32 = 1;

pub struct Signal {
    pub id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub actor: Actor,                     // source + native_id + display_name + resolved_user_id
    pub target: SignalTarget,             // Pattern { name, scope } | NewDraftPattern { payload }
    pub kind: SignalKind,                 // 5 variants below
    pub scope: Scope,
    pub confidence: f64,                  // default 1.0
    pub schema_version: u32,              // default 1
}

pub enum SignalKind {
    ExecutionSuccess,                              // C1
    ExecutionFailure { error: String },            // C1
    UserOverrideAtBreakpoint { reason: Option<String> },  // C1 (3x weight at scoring)
    AutoFixApplied { step: String },               // C1
    NewPatternProposal { origin_context: String }, // C2 + C3
}
```

**Channel mapping** (no new variants needed):

| Channel | SignalKind | SignalTarget | Notes |
|---|---|---|---|
| C1 evidence (success) | `ExecutionSuccess` | `Pattern` | per pattern used in step |
| C1 evidence (failure) | `ExecutionFailure` | `Pattern` | `error` field carries reason |
| C1 evidence (override) | `UserOverrideAtBreakpoint` | `Pattern` | 3× weight applied server-side |
| C1 autofix | `AutoFixApplied` | `Pattern` | signals pattern inadequacy |
| C2 chat extraction | `NewPatternProposal` | `NewDraftPattern { payload }` | `origin_context` = chat source |
| C3 procedural | `NewPatternProposal` | `NewDraftPattern { payload }` | `origin_context` = AuditStore reference |

### Freeze enforcement

Add to `mur-common/src/signal.rs`:

```rust
// ─── FROZEN SCHEMA — v1 ──────────────────────────────────────────────────
// This module is the canonical wire format between commander and mur.
// SCHEMA FREEZE DATE: 2026-05-18
// See: docs/superpowers/specs/2026-05-18-commander-feedback-wire-protocol-design.md
//
// Changes to Signal, SignalKind, SignalTarget, or SIGNAL_SCHEMA_VERSION
// require:
//   1. Bumping SIGNAL_SCHEMA_VERSION to 2
//   2. Coordinated update in the commander repo (closed-source)
//   3. Adding a v2 HTTP endpoint at /v2/signals/...
//   4. Migration plan in a new design spec
// ─────────────────────────────────────────────────────────────────────────
```

And a snapshot test ensuring serialization stays byte-stable across refactors (see §8).

## §3 HTTP Wire Protocol

### §3.1 Endpoints

```
POST   /v1/signals/batch         ← commander → mur (push)
GET    /v1/signals/pending       ← mur → server (pull, used by `mur fetch`)
GET    /v1/signals/dedup-cache   ← debug only, returns recent IDs (admin token)
```

This spec focuses on the push path (`POST /v1/signals/batch`). The pull path predates this spec and is unchanged.

### §3.2 Request

```http
POST /v1/signals/batch HTTP/1.1
Host: 127.0.0.1:7878
Authorization: Bearer <token>            ← required in v1
Content-Type: application/json | application/mur.signed-envelope+json
Content-Length: …

{
  "batch_id": "01988fd8-3b00-7c0b-9a4c-3e8e8c0f4a11",
  "schema_version": 1,
  "signals": [
    { … Signal v1 JSON … },
    { … Signal v1 JSON … },
    …
  ]
}
```

`batch_id` is a UUID owned by the sender — useful for log correlation and at-most-once HTTP delivery (sender retries with same batch_id → server returns cached response).

`Content-Type: application/mur.signed-envelope+json` body shape:

```json
{
  "payload": "<base64-of-canonical-JSON-batch>",
  "sig": "<base64-of-ed25519-signature>",
  "key_version": 1,
  "bridge_pubkey_multibase": "z…"
}
```

The signature covers the **base64 payload bytes** (re-use `mur-common::bridge::envelope::sign_payload`). Verifier MUST NOT re-canonicalize on receive — verify the exact stored bytes. This matches the existing C7 Slack bridge envelope rules.

### §3.3 Response

**Success (all signals accepted or deduplicated):**

```http
HTTP/1.1 202 Accepted
Content-Type: application/json

{
  "batch_id": "01988fd8-…",
  "accepted": 12,
  "deduplicated": 3,
  "received_at": "2026-05-18T10:34:11.412Z"
}
```

**Failure (whole batch rejected):**

```http
HTTP/1.1 400 Bad Request | 401 | 413 | 422
Content-Type: application/json

{
  "batch_id": "01988fd8-…",
  "error": {
    "code": "schema_version_mismatch" | "auth_failed" | "batch_too_large" |
            "signal_invalid" | "signature_mismatch" | "untrusted_peer",
    "message": "human-readable detail",
    "offending_signal_id": "…"          ← only for signal_invalid
  }
}
```

**Status codes:**

| Code | When |
|---|---|
| 202 | Batch accepted (incl. dedup). Always 202, never 200, to signal async processing. |
| 400 | Malformed JSON / missing required fields. |
| 401 | Bearer token absent / invalid. |
| 413 | Batch exceeds 1 MiB or > 1000 signals. |
| 422 | Schema-valid but semantic reject (e.g. unknown schema_version, signature mismatch, untrusted peer). |
| 500 | Server-side bug (inbox write failure). Client SHOULD retry with same batch_id. |
| 503 | Server overloaded or inbox quota exhausted. Client SHOULD back off. |

### §3.4 Idempotency

- **Per-signal**: server tracks `signal.id` for 7 days. Replays return `deduplicated++` in response, not an error.
- **Per-batch**: server caches `batch_id` → response for 1 hour. Retry of same `batch_id` returns the cached response (RFC 7231-style safe retry).

Dedup cache lives in `~/.mur/cache/signal-dedup.sqlite` (single-file SQLite, ephemeral). Cache loss → at-worst duplicate evidence application; tolerable because pattern scoring is monotonic and dedup is best-effort.

### §3.5 Limits

| Limit | Default | Rationale |
|---|---|---|
| Max signals per batch | 1000 | Keeps per-request work bounded |
| Max batch body size | 1 MiB | Protects daemon memory on bulk imports |
| Max signal `target.NewDraftPattern.payload` size | 64 KiB | Pattern YAML is text; this is generous |
| Max request rate | 60 batch/min per token | Prevents accidental tight-loop writers |
| Retention of accepted signals in inbox | until sleep cycle drain | Default 15 min upper bound |

All configurable via `~/.mur/config.yaml` `commander_feedback.limits.*`.

## §4 Authentication

### §4.1 Bearer token (v1 baseline)

Token generated on first mur-daemon start:

```
~/.mur/secrets/commander-token.yaml      (0600, never enters either git repo)
```

```yaml
schema_version: 1
token: "<32 random bytes, base64url-encoded>"
created_at: 2026-05-18T10:00:00Z
expires_at: null          # v1: no expiry; v2: rotateable
rotated_from: null        # v2: previous-token grace-period field
```

Commander reads same file (same-machine assumption in v1) and includes in `Authorization: Bearer <token>` on every request.

Rotation: `mur internals rotate commander-token` writes a new token, keeps previous in `rotated_from` for 1 hour to allow commander hot-reload without dropped signals.

### §4.2 Ed25519 envelope (v1 optional, v2 mandatory)

Re-uses `mur-common::bridge::envelope::SignedEnvelope` already shipped for C7 Slack bridge. The signing key is the commander instance's own `AgentIdentity` (commander treats itself as an A2A peer with its own identity).

Verification path on mur side:
1. Decode `SignedEnvelope` from request body.
2. Look up `bridge_pubkey_multibase` in `~/.mur/commander/trusted-peers.yaml`.
3. If unknown → 422 `untrusted_peer`.
4. Verify via `mur_common::bridge::envelope::verify_envelope_with_pubkey`.
5. On success, deserialize inner payload as `SignalBatch`.

Trust list seeded on first commander pairing:

```bash
mur commander trust <pubkey-multibase>      # adds entry to trusted-peers.yaml
mur commander revoke <pubkey-multibase>
mur commander list-trusted
```

## §5 Commander-side Reference Implementation

This section is the contract for the commander team. Code is reference-quality pseudocode in Rust idiom; commander may implement in any language as long as wire bytes match.

### §5.1 LocalBridge struct

```rust
// In commander repo: crates/daemon/src/local_bridge.rs

use mur_common::{Signal, SignalKind, SignalTarget, Actor, ActorSource, Scope};
use uuid::Uuid;
use chrono::Utc;

pub struct LocalBridge {
    outbox_dir: PathBuf,                     // ~/.mur/commander/outbox
    daemon_url: String,                      // http://127.0.0.1:7878
    bearer_token: String,
    identity: Option<AgentIdentity>,         // None in v1 (no signing), Some in v2
    flush_interval: Duration,
    max_batch_size: usize,
}

impl LocalBridge {
    /// Called by workflow engine when a step completes.
    pub fn record_execution(&self, step: &Step, outcome: &Outcome) {
        for pattern_name in step.patterns_used() {
            let signal = Signal {
                id: Uuid::new_v4(),
                emitted_at: Utc::now(),
                actor: self.actor(),
                target: SignalTarget::Pattern {
                    name: pattern_name.to_string(),
                    scope: step.scope(),
                },
                kind: match outcome {
                    Outcome::Success => SignalKind::ExecutionSuccess,
                    Outcome::Failure(e) => SignalKind::ExecutionFailure { error: e.clone() },
                    Outcome::UserOverride(r) => SignalKind::UserOverrideAtBreakpoint { reason: r.clone() },
                    Outcome::AutoFix(s) => SignalKind::AutoFixApplied { step: s.clone() },
                },
                scope: step.scope(),
                confidence: 1.0,
                schema_version: mur_common::signal::SIGNAL_SCHEMA_VERSION,
            };
            self.enqueue(signal);
        }
    }

    /// Called by chat-extraction tool (C2) or AuditStore analyzer (C3).
    pub fn propose_pattern(&self, draft: Pattern, origin: &str) {
        let signal = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: self.actor(),
            target: SignalTarget::NewDraftPattern { payload: Box::new(draft) },
            kind: SignalKind::NewPatternProposal { origin_context: origin.to_string() },
            scope: Scope::Personal,
            confidence: 0.5,                     // drafts start at lower confidence
            schema_version: mur_common::signal::SIGNAL_SCHEMA_VERSION,
        };
        self.enqueue(signal);
    }

    fn actor(&self) -> Actor {
        Actor {
            source: ActorSource::CommanderDaemon,
            native_id: gethostname::gethostname().to_string_lossy().into_owned(),
            display_name: None,
            resolved_user_id: None,
        }
    }

    fn enqueue(&self, signal: Signal) {
        let path = self.outbox_dir.join(format!(
            "{}-{}.yaml",
            signal.emitted_at.format("%Y-%m-%dT%H-%M-%S"),
            signal.id
        ));
        let tmp = self.outbox_dir.join(format!(".{}.tmp", signal.id));
        std::fs::write(&tmp, serde_yaml::to_string(&signal).unwrap()).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        // Background flusher picks up on next tick
    }
}
```

### §5.2 Flusher loop

```rust
async fn flush_loop(bridge: Arc<LocalBridge>) {
    loop {
        tokio::time::sleep(bridge.flush_interval).await;
        let batch = bridge.collect_pending().await;
        if batch.is_empty() { continue; }

        let body = SignalBatch {
            batch_id: Uuid::new_v4(),
            schema_version: SIGNAL_SCHEMA_VERSION,
            signals: batch.clone(),
        };

        let req = reqwest::Client::new()
            .post(format!("{}/v1/signals/batch", bridge.daemon_url))
            .bearer_auth(&bridge.bearer_token)
            .json(&body);

        let req = if let Some(id) = &bridge.identity {
            let json_bytes = serde_json::to_vec(&body).unwrap();
            let env = mur_common::bridge::envelope::sign_payload(json_bytes, id, 1);
            reqwest::Client::new()
                .post(format!("{}/v1/signals/batch", bridge.daemon_url))
                .bearer_auth(&bridge.bearer_token)
                .header("Content-Type", "application/mur.signed-envelope+json")
                .json(&env)
        } else { req };

        match req.send().await {
            Ok(r) if r.status() == 202 => bridge.delete_pending(&batch).await,
            Ok(r) if r.status() == 401 => bridge.refresh_token().await,
            Ok(r) if r.status() == 422 => bridge.quarantine_invalid(&batch, r).await,
            Ok(_) | Err(_) => {
                // Keep batch in outbox, retry next tick with backoff
            }
        }
    }
}
```

### §5.3 Outbox layout (commander-owned)

```
~/.mur/commander/                          ← commander owns this subdirectory
├── outbox/
│   └── 2026-05-18T10-34-11-<uuid>.yaml
├── outbox.ledger                          ← commander state, not synced
└── token-cache.yaml                       ← cached bearer token (mtime → reload)
```

`~/.mur/commander/` is in mur's `.gitignore` (D7) — commander state is not part of mur's knowledge or execution layer.

## §6 MuR-side Receive Path

### §6.1 New HTTP handler

Add to `mur-core/src/server_agents/routes/`:

```rust
// signals.rs (NEW)
use mur_common::signal::Signal;
use crate::sync::inbox::Inbox;

#[derive(Deserialize)]
pub struct SignalBatch {
    pub batch_id: Uuid,
    pub schema_version: u32,
    pub signals: Vec<Signal>,
}

pub async fn post_batch(
    Headers(auth): Headers,
    Body(body): Body,
) -> Result<impl Reply, Rejection> {
    require_bearer(&auth)?;

    let (batch, signed) = match content_type(&headers) {
        ContentType::Json => (serde_json::from_slice::<SignalBatch>(&body)?, false),
        ContentType::SignedEnvelope => {
            let env: SignedEnvelope = serde_json::from_slice(&body)?;
            verify_against_trust_list(&env)?;
            let inner: SignalBatch = serde_json::from_slice(&env.payload)?;
            (inner, true)
        }
    };

    if batch.schema_version != SIGNAL_SCHEMA_VERSION {
        return Err(reject_schema_mismatch(batch.batch_id, batch.schema_version));
    }
    if batch.signals.len() > MAX_BATCH_SIGNALS {
        return Err(reject_too_large(batch.batch_id));
    }

    let inbox = Inbox::default_location()?;
    let dedup = DedupCache::open()?;
    let mut accepted = 0;
    let mut deduped = 0;

    for sig in &batch.signals {
        if !validate_signal(sig).is_ok() {
            return Err(reject_signal_invalid(batch.batch_id, sig.id));
        }
        if dedup.contains(sig.id)? {
            deduped += 1;
            continue;
        }
        inbox.receive(sig)?;
        dedup.insert(sig.id, Duration::from_days(7))?;
        accepted += 1;
    }

    Ok(json!({
        "batch_id": batch.batch_id,
        "accepted": accepted,
        "deduplicated": deduped,
        "received_at": Utc::now(),
    }))
}
```

### §6.2 Sleep cycle integration

E3's `sleep_cycle::drain_inbox()` (continual-learning v2 spec §6.2.2 step 1) calls existing `Inbox::apply_all(&store)`. No change needed in inbox/apply logic.

Curator (E2 §5.2.2) wraps the apply step with the new versioning store (E1 `VersionedYamlStore`), producing one git commit per applied signal with reason `feedback(c1): commander <actor> <kind>` etc.

## §7 Compatibility & Versioning Rules

| Change type | Allowed without bump | Requires bump | Notes |
|---|---|---|---|
| Add new `SignalKind` variant | ❌ | ✅ → v2 | Old clients can't deserialize new variants |
| Add new field to `Signal` with `#[serde(default)]` | ✅ | — | Forward-compatible additive |
| Add new field without default | ❌ | ✅ → v2 | Breaks deserialization on old clients |
| Add new `ActorSource` variant | ❌ | ✅ → v2 | Same reason |
| Rename field via `#[serde(alias = ...)]` (read both names) | ⚠️ allowed but discouraged | — | Hard to audit; prefer no-rename |
| Change `Pattern` schema in `NewDraftPattern.payload` | governed by `Pattern.schema` field | — | Independent versioning, see knowledge.rs |
| Add new HTTP endpoint at `/v1/...` | ✅ | — | URL is the version |
| Change response shape of existing `/v1/...` endpoint | ❌ | ✅ → /v2/... | New endpoint with new URL |
| Tighten validation (reject previously-accepted input) | ❌ | ✅ → v2 | Breaks existing senders |
| Loosen validation (accept previously-rejected input) | ✅ | — | Backward-compat |

**Process for v1 → v2 bump** (future):

1. Open design spec referencing this doc.
2. Bump `SIGNAL_SCHEMA_VERSION = 2` in `signal.rs`.
3. Add `/v2/signals/batch` handler keeping `/v1/signals/batch` working in parallel for ≥ 90 days.
4. Coordinated PR in commander repo to support v2 wire format.
5. Deprecation warning emitted by `/v1/signals/batch` for last 30 days.

## §8 Test Fixtures

### §8.1 Canonical YAML sample (C1 execution success)

```yaml
id: 01988fd8-3b00-7c0b-9a4c-3e8e8c0f4a11
emitted_at: 2026-05-18T10:34:11.412Z
actor:
  source: commander_daemon
  native_id: host-prod-1.example.com
target:
  kind: pattern
  name: rust-error-handling
  scope:
    kind: personal
kind:
  type: execution_success
scope:
  kind: personal
confidence: 1.0
schema_version: 1
```

### §8.2 Canonical YAML sample (C2 new draft proposal)

```yaml
id: 01988fd8-4400-7000-8aaa-aaaaaaaaaaaa
emitted_at: 2026-05-18T11:00:00Z
actor:
  source: slack
  native_id: U07A9C8DEFG
  display_name: alice
target:
  kind: new_draft_pattern
  payload:
    name: prefer-pnpm-over-npm
    description: Use pnpm for monorepos, npm for single-package projects
    schema: 2
    tier: session
    content:
      plain: |
        Use pnpm in this org. npm causes peer-dep churn in our monorepo.
    importance: 0.5
    tags: [tooling, node]
kind:
  type: new_pattern_proposal
  origin_context: "slack:#dev-platform thread ts=1747560000.000100"
scope:
  kind: personal
confidence: 0.5
schema_version: 1
```

### §8.3 Required snapshot test (to be added to `mur-common`)

```rust
// mur-common/tests/wire_format_snapshot.rs

#[test]
fn signal_v1_wire_format_is_frozen() {
    let signal: Signal = serde_yaml::from_str(include_str!("fixtures/c1_execution_success.yaml")).unwrap();
    let re = serde_yaml::to_string(&signal).unwrap();
    insta::assert_snapshot!("c1_execution_success", re);
}

#[test]
fn signal_v1_unknown_field_strict_reject() {
    let yaml = r#"
id: 01988fd8-3b00-7c0b-9a4c-3e8e8c0f4a11
emitted_at: 2026-05-18T10:34:11Z
actor: { source: commander_daemon, native_id: x }
target: { kind: pattern, name: x, scope: { kind: personal } }
kind: { type: execution_success }
scope: { kind: personal }
new_future_field: "this should reject under v1 strict mode"
"#;
    // Currently serde_yaml ignores unknown fields by default. Spec §7 says
    // unknown variants → v2 bump, but unknown FIELDS are silently ignored
    // (forward-compat additive). This test documents the current behavior.
    let s: Result<Signal, _> = serde_yaml::from_str(yaml);
    assert!(s.is_ok());
}
```

### §8.4 HTTP integration test (to be added to `mur-core`)

```rust
// mur-core/tests/signals_endpoint.rs

#[tokio::test]
async fn post_batch_accepts_and_dedups() {
    let server = test_daemon().await;
    let token = server.commander_token();

    let batch = SignalBatch {
        batch_id: Uuid::new_v4(),
        schema_version: 1,
        signals: vec![sample_signal(), sample_signal()],   // 2 distinct
    };

    // first POST: 202, accepted=2
    let r1 = server.post("/v1/signals/batch", &batch, &token).await;
    assert_eq!(r1.status(), 202);
    assert_eq!(r1.json::<Resp>().await.accepted, 2);

    // second POST same batch_id: cached response
    let r2 = server.post("/v1/signals/batch", &batch, &token).await;
    assert_eq!(r2.status(), 202);

    // third POST with new batch_id but same signal ids: deduplicated
    let batch3 = SignalBatch { batch_id: Uuid::new_v4(), ..batch.clone() };
    let r3 = server.post("/v1/signals/batch", &batch3, &token).await;
    assert_eq!(r3.json::<Resp>().await.deduplicated, 2);
}
```

## §9 Open Questions

1. **Where does the dedup SQLite live during dev?** `~/.mur/cache/signal-dedup.sqlite` is the proposal. Alternative: in-memory only (acceptable since dedup is best-effort). **Proposed answer**: SQLite for prod (survives daemon restart, useful during commander reconnect storms); in-memory for `cargo test` (overrideable via env var).
2. **Should mur-daemon expose a Prometheus metric for accepted/deduplicated/rejected counts?** **Proposed**: yes, behind `mur-core/src/server_agents/metrics.rs` (already exists for B0 telemetry). Names: `mur_signals_batch_accepted_total`, `_deduplicated_total`, `_rejected_total{reason="..."}`.
3. **Multi-tenant token model.** v1 has one token shared with commander. What about future support for Slack bridge writing C2 signals directly (without going through commander)? **Proposed**: defer to v2. v1 path: Slack bridge writes to commander, commander forwards via this protocol.
4. **What about a signal flowing in the other direction (mur → commander, e.g., "this pattern just got Stable, you may now reference it")?** **Proposed**: out of scope for this spec. Existing `mur-core::evolve::commander_bridge` writes workflow YAML to `~/.mur/workflows/` and commander polls. If push notification needed, separate spec.

## §10 Non-goals

- **End-to-end encryption** beyond Ed25519 signing — TLS on a different machine is v2.
- **Multi-host fan-out** — v1 is single-machine. Commander running on a different host requires v2 work (TLS, IP allowlist, mutual auth).
- **Replay attack mitigation** beyond dedup window — assumes localhost is not a hostile network in v1.
- **Schema evolution tooling** — manual coordination is fine while we have one consumer.

## §11 Implementation Checklist

For mur side (this repo):

- [ ] Add freeze annotation to `mur-common/src/signal.rs` (§2)
- [ ] Add `SignalBatch` type to `mur-common/src/signal.rs`
- [ ] Add `mur-common/tests/wire_format_snapshot.rs` with insta snapshots (§8.3)
- [ ] Implement `mur-core/src/server_agents/routes/signals.rs` (§6.1)
- [ ] Implement dedup cache (`mur-core/src/sync/dedup.rs` + SQLite migration)
- [ ] Implement `~/.mur/secrets/commander-token.yaml` generation on daemon first start
- [ ] Add `mur internals rotate commander-token` CLI
- [ ] Add `mur commander trust|revoke|list-trusted` CLI for §4.2
- [ ] Integration test `mur-core/tests/signals_endpoint.rs` (§8.4)
- [ ] Wire E3 `sleep_cycle::drain_inbox()` to existing `Inbox::apply_all()` (continual-learning v2 spec §6.2.2)
- [ ] Add metrics counters (§9 Q2)
- [ ] Docs page: "Commander integration — connecting Commander to MuR"

For commander side (separate repo, parallel work):

- [ ] Add `mur-common` as dep (already may be; confirm version pin)
- [ ] Implement `crates/daemon/src/local_bridge.rs` per §5.1
- [ ] Implement flusher loop per §5.2
- [ ] Outbox directory at `~/.mur/commander/outbox/` per §5.3
- [ ] Read bearer token from `~/.mur/secrets/commander-token.yaml`
- [ ] Optional: implement Ed25519 signing pass once commander has identity
- [ ] Wire workflow engine: every step completion → `LocalBridge::record_execution`
- [ ] Wire chat-extraction tool → `LocalBridge::propose_pattern` (C2)
- [ ] Wire AuditStore analyzer → `LocalBridge::propose_pattern` (C3)
- [ ] Outbox retention: 7 days uncategorized, then archive/discard

## §12 References

- `docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md` — original C1/C2/C3 design
- `docs/superpowers/plans/2026-05-18-mur-commander-channel1-closure.md` — implementation plan that this spec replaces
- `plans/2026-05-18-continual-learning-versioned-evolution.md` — v2 strategy spec (E5, D4, D8)
- `docs/superpowers/specs/2026-05-18-mur-agent-manifest-design.md` — manifest spec (orthogonal but uses same auth model)
- `mur-common/src/signal.rs` — the frozen schema, source of truth
- `mur-common/src/bridge/envelope.rs` — SignedEnvelope (C7 Slack bridge already in production use)
- `mur-core/src/sync/inbox.rs` — receive-side, live
- RFC 7231 §4.2.2 (Idempotent Methods)
- RFC 7807 (Problem Details for HTTP APIs) — error response shape inspiration
