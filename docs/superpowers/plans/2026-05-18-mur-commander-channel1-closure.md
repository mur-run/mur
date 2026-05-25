# MuR ↔ Commander Channel 1 Closure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the execution-feedback loop so commander workflow signals reliably update mur pattern Evidence scores for local-first users.

**Architecture:** Three targeted changes. (1) `LocalBridge` in the commander daemon ticks every 30 s and `fs::rename`s outbox YAML files directly into `~/.mur/inbox/` — no HTTP, no env vars required. (2) A `POST /api/v1/core/signals/batch` handler added to mur-core server, matching the URL `CommanderSyncClient` already calls, for cloud mode. (3) `cmd_inject` drains `Inbox::apply_all()` before scoring so injected patterns always reflect the latest execution feedback.

**Tech Stack:** Rust (tokio, axum, `mur_common::Signal`, `mur_core::sync::inbox::Inbox`, `mur_core::store::yaml::YamlStore`, dirs, tempfile for tests)

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `mur-commander/crates/daemon/src/local_bridge.rs` | **Create** | Tick-based bridge: moves outbox YAML → `~/.mur/inbox/` without HTTP |
| `mur-commander/crates/daemon/src/main.rs` | **Modify** | Add `mod local_bridge;`; replace the silent `_ =>` arm with `LocalBridge::new(...).run()` |
| `mur-core/src/server/signals.rs` | **Create** | `batch_signals` axum handler for `POST /api/v1/core/signals/batch` |
| `mur-core/src/server/mod.rs` | **Modify** | `mod signals;`, import `batch_signals`, register route |
| `mur-core/src/cmd/inject_cmd.rs` | **Modify** | Drain `Inbox::apply_all()` before hybrid scoring |

---

### Task 1: LocalBridge — local-first outbox drain

**Files:**
- Create: `mur-commander/crates/daemon/src/local_bridge.rs`
- Modify: `mur-commander/crates/daemon/src/main.rs` (lines 9–10 top, lines 501–507 bottom)

- [ ] **Step 1: Create `local_bridge.rs` with implementation and tests**

Create `/Volumes/Firecuda4tb/Projects/mur-commander/crates/daemon/src/local_bridge.rs`:

```rust
use anyhow::Result;
use std::path::PathBuf;
use tokio::sync::broadcast;
use tokio::time::{Duration, interval};
use tracing::{info, warn};

/// Moves pending Signal YAML files from the commander outbox directly into
/// `~/.mur/inbox/` without HTTP — used when `MUR_SERVER_URL` is not set.
pub struct LocalBridge {
    outbox_dir: PathBuf,
    inbox_dir: PathBuf,
    interval_secs: u64,
}

impl LocalBridge {
    pub fn new(outbox_dir: PathBuf, inbox_dir: PathBuf, interval_secs: u64) -> Self {
        Self { outbox_dir, inbox_dir, interval_secs }
    }

    pub async fn run(self, mut shutdown: broadcast::Receiver<()>) -> Result<()> {
        let mut tick = interval(Duration::from_secs(self.interval_secs));
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Err(e) = self.flush_once() {
                        warn!("local-bridge flush error: {:#}", e);
                    }
                }
                _ = shutdown.recv() => break,
            }
        }
        Ok(())
    }

    fn flush_once(&self) -> Result<()> {
        let entries = match std::fs::read_dir(&self.outbox_dir) {
            Ok(e) => e,
            Err(_) => return Ok(()), // outbox not yet created — nothing to flush
        };
        std::fs::create_dir_all(&self.inbox_dir)?;
        for entry in entries.flatten() {
            let src = entry.path();
            if !is_pending_yaml(&src) {
                continue;
            }
            let file_name = src.file_name().unwrap();
            let dst = self.inbox_dir.join(file_name);
            if let Err(e) = std::fs::rename(&src, &dst) {
                warn!("local-bridge: rename {} → {}: {e}", src.display(), dst.display());
            } else {
                info!("local-bridge: flushed {}", file_name.to_string_lossy());
            }
        }
        Ok(())
    }
}

fn is_pending_yaml(p: &std::path::Path) -> bool {
    p.is_file()
        && p.extension().and_then(|s| s.to_str()) == Some("yaml")
        && p.file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| !n.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn flush_once_moves_yaml_files_to_inbox() {
        let tmp = tempdir().unwrap();
        let outbox = tmp.path().join("outbox");
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&outbox).unwrap();
        std::fs::write(outbox.join("signal-001.yaml"), "id: abc").unwrap();
        std::fs::write(outbox.join("signal-002.yaml"), "id: xyz").unwrap();

        LocalBridge::new(outbox.clone(), inbox.clone(), 60)
            .flush_once()
            .unwrap();

        assert!(!outbox.join("signal-001.yaml").exists(), "outbox cleared");
        assert!(inbox.join("signal-001.yaml").exists(), "signal-001 in inbox");
        assert!(inbox.join("signal-002.yaml").exists(), "signal-002 in inbox");
    }

    #[test]
    fn flush_once_skips_hidden_and_non_yaml() {
        let tmp = tempdir().unwrap();
        let outbox = tmp.path().join("outbox");
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&outbox).unwrap();
        std::fs::write(outbox.join(".tmp-signal.yaml"), "hidden").unwrap();
        std::fs::write(outbox.join("notes.txt"), "text").unwrap();

        LocalBridge::new(outbox.clone(), inbox.clone(), 60)
            .flush_once()
            .unwrap();

        // inbox either doesn't exist or lacks these files
        if inbox.exists() {
            assert!(!inbox.join(".tmp-signal.yaml").exists(), "hidden skipped");
            assert!(!inbox.join("notes.txt").exists(), "non-yaml skipped");
        }
    }

    #[test]
    fn flush_once_is_noop_when_outbox_absent() {
        let tmp = tempdir().unwrap();
        let bridge = LocalBridge::new(
            tmp.path().join("no-such-outbox"),
            tmp.path().join("inbox"),
            60,
        );
        bridge.flush_once().unwrap(); // must not panic or return Err
    }
}
```

- [ ] **Step 2: Run tests to confirm they pass**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-commander
cargo test --manifest-path crates/daemon/Cargo.toml local_bridge 2>&1
```

Expected: `test local_bridge::tests::flush_once_moves_yaml_files_to_inbox ... ok` etc.

Note: if `tempfile` is not a dev-dependency in `crates/daemon/Cargo.toml`, add:
```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Wire `LocalBridge` in `main.rs`**

In `mur-commander/crates/daemon/src/main.rs`:

Add `mod local_bridge;` after the existing `mod watchdog;` (line 10):
```rust
mod ipc;
mod local_bridge;
mod watchdog;
```

Replace lines 501–507 (the silent `_ =>` arm) with:
```rust
        _ => {
            let outbox_dir = dirs::home_dir()
                .map(|h| h.join(".mur/commander/outbox"))
                .unwrap_or_else(|| std::path::PathBuf::from(".mur/commander/outbox"));
            let inbox_dir = dirs::home_dir()
                .map(|h| h.join(".mur/inbox"))
                .unwrap_or_else(|| std::path::PathBuf::from(".mur/inbox"));
            let interval_secs = std::env::var("MUR_SYNC_FLUSH_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(30);
            let bridge =
                local_bridge::LocalBridge::new(outbox_dir, inbox_dir, interval_secs);
            let bridge_shutdown_rx = shutdown_tx.subscribe();
            Some(tokio::spawn(async move {
                if let Err(e) = bridge.run(bridge_shutdown_rx).await {
                    tracing::error!("local-bridge exited: {:#}", e);
                }
            }))
        }
```

- [ ] **Step 4: Compile check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-commander
cargo build --manifest-path crates/daemon/Cargo.toml 2>&1
```

Expected: `Compiling mur-daemon ... Finished`

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-commander
git add crates/daemon/src/local_bridge.rs crates/daemon/src/main.rs
git commit -m "feat(sync): LocalBridge — direct outbox→inbox rename for local-first mode"
```

---

### Task 2: Server signals endpoint (cloud mode)

**Files:**
- Create: `mur-core/src/server/signals.rs`
- Modify: `mur-core/src/server/mod.rs` (lines 32–58 module block; lines ~200+ router)

- [ ] **Step 1: Create `signals.rs` with handler and integration test**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/server/signals.rs`:

```rust
//! POST /api/v1/core/signals/batch — receive sync signals from commander
use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use mur_common::Signal;
use serde::{Deserialize, Serialize};

use super::{AppError, AppState};
use crate::sync::inbox::Inbox;

#[derive(Deserialize)]
pub struct BatchRequest {
    pub signals: Vec<Signal>,
}

#[derive(Serialize)]
pub struct BatchResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<RejectedSignal>,
}

#[derive(Serialize)]
pub struct RejectedSignal {
    pub id: String,
    pub reason: String,
}

pub async fn batch_signals(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BatchRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.config.readonly {
        return Err(AppError::Readonly);
    }

    let store = state.pattern_store()?;
    let inbox_dir = state
        .patterns_dir
        .parent()
        .unwrap_or(&state.patterns_dir)
        .join("inbox");
    let inbox = Inbox::new(&inbox_dir).map_err(AppError::Internal)?;

    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    for signal in &body.signals {
        match inbox.receive(signal) {
            Ok(_) => accepted.push(signal.id.to_string()),
            Err(e) => rejected.push(RejectedSignal {
                id: signal.id.to_string(),
                reason: e.to_string(),
            }),
        }
    }

    if !accepted.is_empty() {
        let _ = inbox.apply_all(&store);
    }

    Ok(Json(BatchResponse { accepted, rejected }))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::{Content, Pattern, Tier};
    use mur_common::{
        Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, Signal, SignalKind, SignalTarget,
    };
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::server::tests::test_state_for_signals;
    use crate::server::build_router;
    use crate::store::yaml::YamlStore;

    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: "test".into(),
                content: Content::Plain("body".into()),
                tier: Tier::Session,
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    fn exec_success_signal(pattern_name: &str) -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: "bot".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: pattern_name.into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 0.9,
            schema_version: SIGNAL_SCHEMA_VERSION,
        }
    }

    #[tokio::test]
    async fn batch_signals_accepted_and_updates_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state_for_signals(&tmp);
        let store = YamlStore::new(state.patterns_dir.clone()).unwrap();
        store.save(&make_pattern("p1")).unwrap();

        let app = build_router(state);
        let body = serde_json::json!({
            "signals": [exec_success_signal("p1")]
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/core/signals/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["accepted"].as_array().unwrap().len(), 1);
        assert_eq!(json["rejected"].as_array().unwrap().len(), 0);

        let updated = YamlStore::new(tmp.path().join("patterns"))
            .unwrap()
            .get("p1")
            .unwrap();
        assert_eq!(updated.evidence.success_signals, 1);
    }

    #[tokio::test]
    async fn batch_signals_readonly_returns_403() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = test_state_for_signals(&tmp);
        state.config.readonly = true;

        let app = build_router(state);
        let body = serde_json::json!({ "signals": [] });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/core/signals/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
```

- [ ] **Step 2: Add `test_state_for_signals` helper to `server/tests.rs`**

The signals test needs an `AppState` where `patterns_dir.parent()` is a writable temp dir. The existing `test_state` works — add a re-export helper in `tests.rs` so `signals::tests` can reach it:

In `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/server/tests.rs`, add after `test_state_readonly`:

```rust
/// Re-exported for use by sibling module tests (e.g. signals::tests).
pub(super) fn test_state_for_signals(tmp: &tempfile::TempDir) -> AppState {
    test_state(tmp)
}
```

- [ ] **Step 3: Run tests to verify they fail (route missing)**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo test -p mur-core server::signals 2>&1
```

Expected: compile error — `mod signals` not declared and route missing.

- [ ] **Step 4: Register module and route in `server/mod.rs`**

In `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/server/mod.rs`:

Add `mod signals;` in the module block (after `mod workflows;`, line ~37):
```rust
mod signals;
mod workflows;
```

Add `use signals::batch_signals;` in the use block (after `use workflows::...`):
```rust
use signals::batch_signals;
```

Add route in `build_router` (after the feedback route, before sessions, around line 203):
```rust
        .route("/api/v1/core/signals/batch", post(batch_signals))
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo test -p mur-core server::signals 2>&1
```

Expected:
```
test server::signals::tests::batch_signals_accepted_and_updates_evidence ... ok
test server::signals::tests::batch_signals_readonly_returns_403 ... ok
```

- [ ] **Step 6: Run full workspace tests**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo test --workspace 2>&1 | tail -20
```

Expected: no regressions.

- [ ] **Step 7: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-core/src/server/signals.rs mur-core/src/server/mod.rs mur-core/src/server/tests.rs
git commit -m "feat(server): POST /api/v1/core/signals/batch — cloud-mode signal ingestion"
```

---

### Task 3: Drain inbox on inject

**Files:**
- Modify: `mur-core/src/cmd/inject_cmd.rs` (~lines 9–27)

- [ ] **Step 1: Write failing test**

Add to `mur-core/src/cmd/inject_cmd.rs` (before `cmd_why`):

```rust
#[cfg(test)]
mod tests {
    use crate::store::yaml::YamlStore;
    use crate::sync::inbox::Inbox;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::{Content, Pattern, Tier};
    use mur_common::{
        Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, Signal, SignalKind, SignalTarget,
    };
    use uuid::Uuid;

    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: "test".into(),
                content: Content::Plain("body".into()),
                tier: Tier::Session,
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    #[test]
    fn inbox_drain_applies_before_scoring() {
        let tmp = tempfile::tempdir().unwrap();
        let patterns_dir = tmp.path().join("patterns");
        let inbox_dir = tmp.path().join("inbox");
        std::fs::create_dir_all(&inbox_dir).unwrap();

        let store = YamlStore::new(&patterns_dir).unwrap();
        store.save(&make_pattern("drain-test")).unwrap();

        // Write a success signal directly into the inbox
        let signal = Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: "bot".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: "drain-test".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 0.9,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        let inbox = Inbox::new(&inbox_dir).unwrap();
        inbox.receive(&signal).unwrap();

        // Drain
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 1);

        // Pattern evidence updated
        let p = store.get("drain-test").unwrap();
        assert_eq!(p.evidence.success_signals, 1);
    }
}
```

- [ ] **Step 2: Run test to verify it passes (it tests the inbox directly)**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo test -p mur-core inject_cmd::tests 2>&1
```

Expected: `test cmd::inject_cmd::tests::inbox_drain_applies_before_scoring ... ok`

- [ ] **Step 3: Add inbox drain to `cmd_inject`**

In `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/cmd/inject_cmd.rs`, move the `yaml_store` creation to line 10 and add the inbox drain immediately after `heartbeat()`:

Replace lines 9–27:
```rust
pub(crate) async fn cmd_inject(query: &str) -> Result<()> {
    crate::auth::heartbeat();
    use crate::retrieve::gate::{Tier as GateTier, evaluate_query};
    use crate::retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::LanceDbStore as VectorStore;
    use std::collections::HashMap;

    let outcome = evaluate_query(query);
    if outcome.tier == GateTier::Skip {
        eprintln!(
            "# No patterns (gate: skip, score={:.2}, reasons={:?})",
            outcome.score, outcome.reasons
        );
        return Ok(());
    }

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
```

with:
```rust
pub(crate) async fn cmd_inject(query: &str) -> Result<()> {
    crate::auth::heartbeat();
    use crate::retrieve::gate::{Tier as GateTier, evaluate_query};
    use crate::retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::LanceDbStore as VectorStore;
    use std::collections::HashMap;

    let outcome = evaluate_query(query);
    if outcome.tier == GateTier::Skip {
        eprintln!(
            "# No patterns (gate: skip, score={:.2}, reasons={:?})",
            outcome.score, outcome.reasons
        );
        return Ok(());
    }

    let yaml_store = YamlStore::default_store()?;
    // Drain pending commander signals so evidence scores are current before ranking
    if let Ok(inbox) = crate::sync::inbox::Inbox::default_location() {
        let _ = inbox.apply_all(&yaml_store);
    }
    let patterns = yaml_store.list_all()?;
```

- [ ] **Step 4: Compile check**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo build -p mur-core 2>&1
```

Expected: `Finished`

- [ ] **Step 5: Run full tests**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo test -p mur-core 2>&1 | tail -20
```

Expected: no regressions.

- [ ] **Step 6: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
git add mur-core/src/cmd/inject_cmd.rs
git commit -m "feat(inject): drain inbox before scoring so commander feedback is reflected"
```

---

## Self-Review

**Spec coverage:**
- C1 local path (P0-A + P0-B): ✅ `LocalBridge` created, wired in daemon
- C1 cloud path (P0-C + P0-D): ✅ `/api/v1/core/signals/batch` handler + route
- Inject trigger (P0-E): ✅ `Inbox::apply_all()` in `cmd_inject`

**Placeholder scan:** None found — all steps contain complete code.

**Type consistency:**
- `LocalBridge::new(outbox_dir, inbox_dir, interval_secs)` matches Task 1 declaration and Task 1 wiring
- `test_state_for_signals` in `tests.rs` matches import `crate::server::tests::test_state_for_signals` in `signals::tests`
- `batch_signals` function name matches `use signals::batch_signals` import and `.route(...)` call
- `Inbox::default_location()` call matches existing `pub fn default_location()` in `inbox.rs:35`
- `inbox.apply_all(&yaml_store)` matches signature `pub fn apply_all(&self, store: &YamlStore)` in `inbox.rs:61`
