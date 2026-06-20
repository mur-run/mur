# Commander Node-Side Receiver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two `mur-core` HTTP endpoints so the closed Commander engine can deliver a signed governance directive to this node and read back the compliance audit.

**Architecture:** Two axum handlers in a new `mur-core/src/server/governance.rs`, wired into the existing dashboard router (`server/mod.rs`). The POST handler verifies the commander's Ed25519 signature, then persists the *pre-signed* `ChannelEvent` verbatim via `ChannelStore::append_event` (no re-sign) so the existing `fold_governance` enforces it; it also writes a `received` audit row. The GET handler returns the node's `Governance` audit entries. No daemon, no new transport, no new dependency.

**Tech Stack:** Rust 2024, axum, tokio, `mur_channel` (ChannelStore/sign), `sha2`+`hex` (already deps of mur-core), `cargo nextest`.

## Global Constraints

- Endpoints (versioned, exact): `POST /api/v1/governance/directive`, `GET /api/v1/governance/audit/{fleet}`. The engine PR (`feature/commander-engine-v1`) updates its path constants to match.
- Reuse the existing `require_auth` middleware (bearer = `MUR_SERVER_TOKEN`) — add NO new auth code.
- `channel_id` is ALWAYS `format!("fleet-{fleet}")` derived from the signed `payload.commander_directive.fleet`, never taken from the request body.
- Persist with `ChannelStore::append_event` (forwards the commander's `sig`/`key_version`). Never use `ChannelService::append` (drops sig) or `append_signed` (re-signs).
- `ChannelStore::append_event` auto-creates the channel dir, so the handler MUST pre-check existence with `load_manifest` and 404 if absent (don't let an attacker-named fleet create a channel).
- `content_sha256 = hex(Sha256(mur_channel::sign::sign_input(channel_id, &actor, kind, &payload, idempotency_key.as_deref())))`.
- Fail-closed: every reject path returns before writing anything.
- Build/test: `export CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download`; run `cargo nextest run -p mur-core -E 'test(governance)'` (do NOT `cargo build --workspace`). Lint: `cargo clippy -p mur-core -- -D warnings`; `cargo fmt`.

---

### Task 1: `audit::read_entries` + `"received"` decision label

**Files:**
- Modify: `mur-core/src/conversations/audit.rs`

**Interfaces:**
- Produces: `pub fn read_entries(root_override: Option<&str>) -> anyhow::Result<Vec<AuditEntry>>` — all chain entries oldest-first; missing file → empty vec; unparseable lines skipped.

- [ ] **Step 1: Write the failing test** (append to the `#[cfg(test)] mod tests` in `audit.rs`)

```rust
#[test]
fn read_entries_roundtrips_and_tolerates_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_str().unwrap();
    assert!(read_entries(Some(root)).unwrap().is_empty()); // missing file → empty
    let audit = Audit::open(Some(root)).unwrap();
    audit
        .append(
            AuditAction::Governance {
                fleet: "dev".into(),
                directive: "kill".into(),
                decision: "received".into(),
                nonce: "n1".into(),
            },
            "abc".into(),
        )
        .unwrap();
    let got = read_entries(Some(root)).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].content_sha256, "abc");
    assert!(matches!(&got[0].action, AuditAction::Governance { nonce, .. } if nonce == "n1"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core -E 'test(read_entries_roundtrips)'`
Expected: FAIL — `cannot find function read_entries`.

- [ ] **Step 3: Implement `read_entries`** (add after the `Audit` impl block, near `read_last_hash`, so the private `audit_path` is in scope)

```rust
/// Read all audit entries (oldest first). Missing file → empty.
pub fn read_entries(root_override: Option<&str>) -> Result<Vec<AuditEntry>> {
    let path = audit_path(root_override);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    // ponytail: skip unparseable lines (matches ChannelStore::load_events); one
    // corrupt historical row must not 500 the compliance endpoint.
    Ok(content
        .lines()
        .filter_map(|l| serde_json::from_str::<AuditEntry>(l).ok())
        .collect())
}
```

- [ ] **Step 4: Update the `Governance` decision doc** — in the `AuditAction::Governance` variant, change the `decision` doc comment to include the new label:

```rust
        decision: String,  // "halted" | "capped" | "fail_closed" | "received"
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo nextest run -p mur-core -E 'test(read_entries_roundtrips)'`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/conversations/audit.rs
git commit -m "feat(server): audit::read_entries + received decision label"
```

---

### Task 2: `POST /api/v1/governance/directive`

**Files:**
- Create: `mur-core/src/server/governance.rs`
- Modify: `mur-core/src/server/mod.rs` (add `mod governance;`, the POST route, `AppError::Forbidden`, `AppState::mur_home()`)

**Interfaces:**
- Consumes: `audit::read_entries` (Task 1); `mur_channel::ChannelStore::{new,load_manifest,load_events,append_event}`; `mur_channel::sign::{sign_input,verify_event_sig}`; `crate::cmd::commander::accepted_pubkeys`; `mur_common::commander::COMMANDER_DIRECTIVE_KEY`; `mur_common::channel::{ChannelActor,EventKind}`.
- Produces: `pub async fn post_directive(State<Arc<AppState>>, Json<DirectiveEnvelope>) -> Result<impl IntoResponse, AppError>`; `AppError::Forbidden(String)`; `AppState::mur_home() -> PathBuf`.

- [ ] **Step 1: Add `AppError::Forbidden` + `AppState::mur_home()`** in `server/mod.rs`

In `enum AppError` add `Forbidden(String),`. In the `IntoResponse` match add:
```rust
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
```
In `impl AppState` add (mirrors `skills_dir()`):
```rust
    /// `~/.mur` — parent of `patterns_dir`.
    pub(super) fn mur_home(&self) -> std::path::PathBuf {
        self.patterns_dir
            .parent()
            .unwrap_or(&self.patterns_dir)
            .to_path_buf()
    }
```

- [ ] **Step 2: Declare the module + route** in `server/mod.rs`

Near the other `mod` lines add `mod governance;`. In the router builder (`build_router_with_auth`, the `Router::new()` chain) add:
```rust
        .route("/api/v1/governance/directive", post(governance::post_directive))
```

- [ ] **Step 3: Write the failing test** — create `mur-core/src/server/governance.rs` with ONLY the test module first (so it compiles to a failing state once the handler is referenced). Put the shared test helpers + the happy-path test here:

```rust
//! Node-side commander governance receiver: POST a signed directive, GET the audit.

// (handler code added in later steps)

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mur_common::channel::{ChannelActor, EventKind};
    use mur_common::commander::COMMANDER_DIRECTIVE_KEY;
    use mur_common::identity::AgentIdentity;
    use std::sync::Arc;
    use tower::ServiceExt;

    // Build AppState (reuses the sibling harness), pin a commander key, create the
    // fleet-dev channel. Returns (state, the pinned identity).
    fn setup(tmp: &tempfile::TempDir) -> (Arc<AppState>, AgentIdentity) {
        let state = crate::server::tests::test_state_for_signals(tmp);
        let home = tmp.path();
        let id = AgentIdentity::generate();
        crate::cmd::commander::cmd_commander_pin(home, &id.public_key_multibase(), false).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &[])
            .unwrap();
        (Arc::new(state), id)
    }

    // Build the JSON body the engine POSTs: { "event": <signed ChannelEvent> }.
    fn directive_body(id: &AgentIdentity, fleet: &str, kind: &str, budget: Option<f64>) -> serde_json::Value {
        let nonce = "11111111-1111-1111-1111-111111111111".to_string();
        let channel_id = format!("fleet-{fleet}");
        let payload = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
            "kind": kind, "fleet": fleet, "budget_usd": budget,
            "nonce": nonce, "issued_at_ms": 1_000_000u64,
        }});
        let actor = ChannelActor::System;
        let sig = mur_channel::sign::sign_event(id, &channel_id, &actor, EventKind::Note, &payload, Some(&nonce));
        serde_json::json!({ "event": {
            "seq": 0,
            "ts": chrono::Utc::now().to_rfc3339(),
            "actor": serde_json::to_value(&actor).unwrap(),
            "kind": serde_json::to_value(EventKind::Note).unwrap(),
            "payload": payload,
            "idempotency_key": nonce,
            "sig": sig,
            "key_version": serde_json::Value::Null,
        }})
    }

    async fn post(state: Arc<AppState>, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        let app = crate::server::build_router((*state).clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/governance/directive")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn valid_kill_is_persisted_audited_and_folds_killed() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp);
        let (status, json) = post(state.clone(), directive_body(&id, "dev", "kill", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["accepted"], serde_json::json!(true));

        let home = tmp.path();
        let events = mur_channel::ChannelService::open(home).unwrap().load_events("fleet-dev").unwrap();
        assert!(events.iter().any(|e| e.payload.get(COMMANDER_DIRECTIVE_KEY).is_some()));

        // fold sees the kill
        let keys = [id.verifying_key_bytes()];
        let gov = mur_channel::governance::fold_governance(&events, "fleet-dev", "dev", &keys);
        assert!(gov.killed);

        // received audit row bound to nonce + content_sha256
        let rows = crate::conversations::audit::read_entries(home.to_str()).unwrap();
        let row = rows.iter().find(|e| matches!(&e.action,
            crate::conversations::audit::AuditAction::Governance { decision, .. } if decision == "received"))
            .expect("received row");
        assert!(!row.content_sha256.is_empty());
    }

    #[tokio::test]
    async fn wrong_key_is_rejected_403_and_persists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _pinned) = setup(&tmp);
        let attacker = AgentIdentity::generate(); // not the pinned key
        let (status, _) = post(state.clone(), directive_body(&attacker, "dev", "kill", None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let events = mur_channel::ChannelService::open(tmp.path()).unwrap().load_events("fleet-dev").unwrap();
        assert!(events.iter().all(|e| e.payload.get(COMMANDER_DIRECTIVE_KEY).is_none()));
    }

    #[tokio::test]
    async fn no_commander_pinned_is_403() {
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(crate::server::tests::test_state_for_signals(&tmp));
        mur_channel::ChannelService::open(tmp.path()).unwrap().create_for_fleet("dev", "mur", &[]).unwrap();
        let id = AgentIdentity::generate(); // signed but no key pinned
        let (status, _) = post(state, directive_body(&id, "dev", "kill", None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unknown_fleet_is_404() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp); // only fleet-dev exists
        let (status, _) = post(state, directive_body(&id, "ghost", "kill", None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn duplicate_post_is_idempotent_single_audit_row() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp);
        let body = directive_body(&id, "dev", "kill", None);
        let (s1, j1) = post(state.clone(), body.clone()).await;
        let (s2, j2) = post(state.clone(), body).await;
        assert_eq!(s1, StatusCode::OK);
        assert_eq!(s2, StatusCode::OK);
        assert_eq!(j1["seq"], j2["seq"]); // same event, same seq
        let rows = crate::conversations::audit::read_entries(tmp.path().to_str()).unwrap();
        let received = rows.iter().filter(|e| matches!(&e.action,
            crate::conversations::audit::AuditAction::Governance { decision, .. } if decision == "received")).count();
        assert_eq!(received, 1); // not double-written
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo nextest run -p mur-core -E 'test(governance)'`
Expected: FAIL — `post_directive`/`DirectiveEnvelope` not found (compile error).

- [ ] **Step 5: Implement the handler** — add above the test module in `governance.rs`:

```rust
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use mur_common::channel::{ChannelActor, EventKind};
use mur_common::commander::COMMANDER_DIRECTIVE_KEY;

use super::{AppError, AppState};

#[derive(Deserialize)]
pub struct DirectiveEnvelope {
    event: serde_json::Value,
}

/// Accept a commander-signed governance directive and persist it verbatim into the
/// fleet channel so `fold_governance` enforces it. Two-layer auth: the network bearer
/// (require_auth middleware) + the commander Ed25519 signature verified here.
pub async fn post_directive(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DirectiveEnvelope>,
) -> Result<impl IntoResponse, AppError> {
    let event = &body.event;
    let payload = event
        .get("payload")
        .ok_or_else(|| AppError::BadRequest("event.payload missing".into()))?
        .clone();
    let fleet = payload
        .get(COMMANDER_DIRECTIVE_KEY)
        .and_then(|d| d.get("fleet"))
        .and_then(|f| f.as_str())
        .ok_or_else(|| AppError::BadRequest("commander_directive.fleet missing".into()))?
        .to_string();
    let channel_id = format!("fleet-{fleet}");
    let mur_home = state.mur_home();

    let actor: ChannelActor = serde_json::from_value(
        event.get("actor").cloned().ok_or_else(|| AppError::BadRequest("event.actor missing".into()))?,
    )
    .map_err(|e| AppError::BadRequest(format!("bad actor: {e}")))?;
    let kind: EventKind = serde_json::from_value(
        event.get("kind").cloned().ok_or_else(|| AppError::BadRequest("event.kind missing".into()))?,
    )
    .map_err(|e| AppError::BadRequest(format!("bad kind: {e}")))?;
    let idem = event.get("idempotency_key").and_then(|v| v.as_str()).map(str::to_string);
    let sig = event
        .get("sig")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::Forbidden("event.sig missing".into()))?;
    let key_version = event.get("key_version").and_then(|v| v.as_u64()).map(|n| n as u32);

    // Commander signature (defense-in-depth; fold re-verifies on read).
    let keys = crate::cmd::commander::accepted_pubkeys(&mur_home);
    if keys.is_empty() {
        return Err(AppError::Forbidden("no commander key pinned".into()));
    }
    let sig_ok = keys.iter().any(|pk| {
        mur_channel::sign::verify_event_sig(&channel_id, &actor, kind, &payload, idem.as_deref(), &sig, pk)
    });
    if !sig_ok {
        return Err(AppError::Forbidden("invalid commander signature".into()));
    }

    // Channel must already exist — append_event auto-creates the dir, so guard here.
    let store = mur_channel::ChannelStore::new(&mur_home);
    if store.load_manifest(&channel_id).is_err() {
        return Err(AppError::NotFound(format!("unknown fleet channel {channel_id}")));
    }

    // Was this nonce already persisted? (don't double-write the receipt audit.)
    let already = idem.as_deref().is_some_and(|k| {
        store
            .load_events(&channel_id)
            .map(|evs| evs.iter().any(|e| e.idempotency_key.as_deref() == Some(k)))
            .unwrap_or(false)
    });

    let ev = store
        .append_event(&channel_id, actor.clone(), kind, payload.clone(), idem.clone(), Some(sig), key_version)
        .map_err(AppError::Internal)?;

    if !already {
        let content = mur_channel::sign::sign_input(&channel_id, &actor, kind, &payload, idem.as_deref());
        let content_sha256 = hex::encode(Sha256::digest(&content));
        let directive = payload
            .get(COMMANDER_DIRECTIVE_KEY)
            .and_then(|d| d.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("")
            .to_string();
        if let Ok(audit) = crate::conversations::audit::Audit::open(mur_home.to_str()) {
            let _ = audit.append(
                crate::conversations::audit::AuditAction::Governance {
                    fleet,
                    directive,
                    decision: "received".into(),
                    nonce: idem.unwrap_or_default(),
                },
                content_sha256,
            );
        }
    }

    Ok(Json(json!({ "accepted": true, "seq": ev.seq })))
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo nextest run -p mur-core -E 'test(governance)'`
Expected: PASS (5 tests).

- [ ] **Step 7: Lint + commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/server/governance.rs mur-core/src/server/mod.rs
git commit -m "feat(server): POST /api/v1/governance/directive receiver"
```

---

### Task 3: `GET /api/v1/governance/audit/{fleet}`

**Files:**
- Modify: `mur-core/src/server/governance.rs` (add `get_audit` + tests)
- Modify: `mur-core/src/server/mod.rs` (add the GET route)

**Interfaces:**
- Consumes: `audit::read_entries` (Task 1); `AuditAction::Governance` matching by `fleet`.
- Produces: `pub async fn get_audit(State<Arc<AppState>>, Path<String>, Query<AuditQuery>) -> Result<impl IntoResponse, AppError>`.

- [ ] **Step 1: Add the GET route** in `server/mod.rs`, next to the POST route:

```rust
        .route("/api/v1/governance/audit/{fleet}", get(governance::get_audit))
```

- [ ] **Step 2: Write the failing test** — append to `governance.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn get_audit_returns_received_row_with_matching_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp);
        let body = directive_body(&id, "dev", "kill", None);
        let _ = post(state.clone(), body).await;

        let app = crate::server::build_router((*state).clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/governance/audit/dev")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entries = json["entries"].as_array().unwrap();
        assert!(entries.iter().any(|e| e["action"]["nonce"]
            == serde_json::json!("11111111-1111-1111-1111-111111111111")));
    }

    #[tokio::test]
    async fn get_audit_since_nonce_filters() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, id) = setup(&tmp);
        let _ = post(state.clone(), directive_body(&id, "dev", "kill", None)).await;
        let app = crate::server::build_router((*state).clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/v1/governance/audit/dev?since_nonce=11111111-1111-1111-1111-111111111111")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["entries"].as_array().unwrap().is_empty()); // the only row is excluded
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo nextest run -p mur-core -E 'test(get_audit)'`
Expected: FAIL — `get_audit` not found.

- [ ] **Step 4: Implement `get_audit`** — add to `governance.rs` (and `use axum::extract::{Path, Query};` to the imports):

```rust
#[derive(Deserialize)]
pub struct AuditQuery {
    since_nonce: Option<String>,
}

/// Return this node's `Governance` audit rows for `fleet` (oldest first). The engine's
/// ComplianceChecker matches by `action.nonce` and recomputes `content_sha256`.
pub async fn get_audit(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(fleet): axum::extract::Path<String>,
    axum::extract::Query(q): axum::extract::Query<AuditQuery>,
) -> Result<impl IntoResponse, AppError> {
    use crate::conversations::audit::AuditAction;
    let all = crate::conversations::audit::read_entries(state.mur_home().to_str())
        .map_err(AppError::Internal)?;
    let mut entries: Vec<_> = all
        .into_iter()
        .filter(|e| matches!(&e.action, AuditAction::Governance { fleet: f, .. } if *f == fleet))
        .collect();
    if let Some(since) = q.since_nonce.as_deref() {
        // Drop everything up to and including the row with this nonce.
        if let Some(pos) = entries.iter().position(|e| matches!(&e.action,
            AuditAction::Governance { nonce, .. } if nonce == since))
        {
            entries.drain(..=pos);
        }
    }
    Ok(Json(json!({ "entries": entries })))
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo nextest run -p mur-core -E 'test(governance)'`
Expected: PASS (7 tests).

- [ ] **Step 6: Lint + commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/server/governance.rs mur-core/src/server/mod.rs
git commit -m "feat(server): GET /api/v1/governance/audit/{fleet} for compliance readback"
```

---

## Notes for the implementer
- `build_router(state)` (no-auth, loopback variant) is used in tests; the real bind uses `build_router_with_auth` — the routes are identical, so the tests exercise the handler logic and the existing `require_auth` tests cover the bearer gate.
- `serde_json::to_value(&ChannelActor::System)` yields `{"kind":"system"}` and `EventKind::Note` yields `"note"`; building the test event through `to_value` (not hand-written JSON) keeps it in lockstep with the real serde shapes.
- Do not touch `mur-channel/src/governance.rs` (`fold_governance`) — it already reads the persisted event.
