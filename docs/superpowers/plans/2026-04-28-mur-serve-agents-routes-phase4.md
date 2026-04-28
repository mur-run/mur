# `mur serve` — Agents Routes (Phase 4) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add read-only HTTP routes under `/api/v1/agents/*` to the existing axum-based `mur serve` so the dashboard SPA (and any other client) can list agents, read profiles, tail telemetry, and inspect eval-run results without shelling out to the `mur agent` CLI.

**Architecture:** Add a new `server_agents/` module under `mur-core/src/`, expose a nested axum router that's mounted by the existing `build_router()` in `server.rs`. All handlers are read-only (Phase 4 scope). Source of truth is `~/.mur/agents/<name>/{profile.yaml,running.lock,telemetry/<date>.jsonl,evals/<run_id>.json}` — no daemon, no socket calls. Live telemetry tail uses a poll-based WS handler over today's JSONL file.

**Tech Stack:** Rust 2024, axum 0.8, `tokio::sync::broadcast`, `serde_yaml_ng`, `serde_json`, `tempfile` for tests, `tower::ServiceExt::oneshot` for handler tests.

**Reference design:** `/Users/david/Projects/mur-agent-harness/phase9/design.md` (sections "Architecture", "Output: ~/.mur/agents/<name>/evals/<run-id>.json", and the "mur serve agent routes" table). This plan implements only the read-only rows of that table (`GET /agents`, `GET /agents/{name}`, `GET telemetry`, `WS stream`, `GET evals`, `GET evals/{run_id}`). Write actions (POST/PUT) are out of scope and will be Phase 6.

**Lost work this recovers:** branch `feat/serve-agents-routes` from `/private/tmp/mur-feat-serve` (worktree wiped on macOS reboot 2026-04-28 07:42, no commits past `main`).

---

## Setup (do once before Task 1)

The implementation must happen in a worktree that lives **under `~/Projects/`, not `/tmp/`** — the previous attempt was lost when macOS cleared `/private/tmp` on reboot.

```bash
cd /Users/david/Projects/mur
git fetch origin
git worktree add -b feat/serve-agents-routes ../mur-feat-serve origin/main
cd ../mur-feat-serve
cargo build -p mur-core --features server   # warm the build cache
```

Verify the worktree path is **not** `/tmp/...`:

```bash
pwd                        # expected: /Users/david/Projects/mur-feat-serve (or similar non-tmp path)
git worktree list          # expected: this path on branch feat/serve-agents-routes
```

All file paths below are relative to the worktree root.

---

## File Structure

| File | Status | Responsibility |
|------|--------|----------------|
| `mur-core/src/server.rs` | MODIFY | Add `agents_dir` field to `AppState`; mount `server_agents::router()` inside `build_router()`. |
| `mur-core/src/server_agents/mod.rs` | NEW | Public `router()` builder, shared helpers (`agent_home`, `is_running`, error mapping). |
| `mur-core/src/server_agents/list.rs` | NEW | `GET /api/v1/agents` handler + tests. |
| `mur-core/src/server_agents/detail.rs` | NEW | `GET /api/v1/agents/{name}` handler + tests. |
| `mur-core/src/server_agents/telemetry.rs` | NEW | `GET /api/v1/agents/{name}/telemetry` handler, `WS /api/v1/agents/{name}/stream` handler + tests. |
| `mur-core/src/server_agents/evals.rs` | NEW | `GET /api/v1/agents/{name}/evals` and `GET /api/v1/agents/{name}/evals/{run_id}` handlers + tests. |
| `mur-core/src/lib.rs` | MODIFY | Add `mod server_agents;` (under the existing `#[cfg(feature = "server")]` gate that wraps `mod server;`). |

Each handler file is self-contained: handler fn(s), response shape struct(s), and a `#[cfg(test)] mod tests` block that uses `tempfile::tempdir()` + `tower::ServiceExt::oneshot` (mirrors the existing pattern at `mur-core/src/server.rs:1489-1520`).

---

## Task 1: Wire `agents_dir` into `AppState` and create empty router skeleton

**Files:**
- Modify: `mur-core/src/server.rs:62-71` (add field), `mur-core/src/server.rs:142-216` (mount nested router), `mur-core/src/server.rs:1497-1514` (update `test_state` helper)
- Create: `mur-core/src/server_agents/mod.rs`
- Modify: `mur-core/src/lib.rs` (add `mod server_agents;` under the `#[cfg(feature = "server")]` block)

### - [ ] Step 1: Write the failing smoke test

Add this test at the bottom of `mur-core/src/server.rs` inside the existing `mod tests` block (just before its closing `}`):

```rust
#[tokio::test]
async fn test_agents_router_mounted_returns_404_for_unknown_subpath() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_router(test_state(&tmp));

    // /api/v1/agents/__nope__ should resolve into the agents subrouter
    // (and fall through to a 404), proving the nested router is mounted.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/agents/__nope__/__nope__")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
```

### - [ ] Step 2: Run the test to confirm it compiles but fails

Run: `cargo test -p mur-core --features server test_agents_router_mounted -- --nocapture`

Expected: PASS (404 is the default for unmatched routes), but if the test does not compile, fix imports first. The point is we have a green baseline before adding the new module.

### - [ ] Step 3: Add the new module to `lib.rs`

Open `mur-core/src/lib.rs`, find the existing `#[cfg(feature = "server")] pub mod server;` line, and add immediately after it:

```rust
#[cfg(feature = "server")]
pub mod server_agents;
```

### - [ ] Step 4: Create the empty router file

Create `mur-core/src/server_agents/mod.rs` with this content:

```rust
//! HTTP routes for `~/.mur/agents/*` — read-only Phase 4.
//!
//! Mounted under `/api/v1/agents` by [`crate::server::build_router`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;

use crate::server::AppState;

pub mod list;
pub mod detail;
pub mod telemetry;
pub mod evals;

/// Build the nested `/api/v1/agents` router. Returns an empty router for
/// now — handlers are wired in by later tasks.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}

// ─── Shared helpers ────────────────────────────────────────────────

/// Absolute path to `~/.mur/agents/<name>/`. Does not check existence.
pub(crate) fn agent_home(agents_dir: &Path, name: &str) -> PathBuf {
    agents_dir.join(name)
}

/// True when `<agent_home>/running.lock` exists. Per the runtime spec
/// the lock is created on supervisor start and removed on clean exit.
pub(crate) fn is_running(home: &Path) -> bool {
    home.join("running.lock").exists()
}
```

### - [ ] Step 5: Create the four (currently empty) handler files

Create each of these with a single doc comment so the module imports compile:

`mur-core/src/server_agents/list.rs`:
```rust
//! `GET /api/v1/agents` — list agents under `~/.mur/agents/`.
```

`mur-core/src/server_agents/detail.rs`:
```rust
//! `GET /api/v1/agents/{name}` — full profile + status.
```

`mur-core/src/server_agents/telemetry.rs`:
```rust
//! `GET /api/v1/agents/{name}/telemetry` and `WS /api/v1/agents/{name}/stream`.
```

`mur-core/src/server_agents/evals.rs`:
```rust
//! `GET /api/v1/agents/{name}/evals[/{run_id}]`.
```

### - [ ] Step 6: Add `agents_dir` field to `AppState`

In `mur-core/src/server.rs:62-71`, change the struct to:

```rust
#[derive(Clone)]
pub struct AppState {
    pub patterns_dir: PathBuf,
    pub workflows_dir: PathBuf,
    pub pipelines_dir: PathBuf,
    /// `~/.mur/agents/` — root for per-agent profiles, telemetry, evals.
    pub agents_dir: PathBuf,
    /// Path to the LanceDB vector index (`~/.mur/index`).
    /// When present the context endpoint uses hybrid scoring.
    pub index_dir: PathBuf,
    pub config: ServerConfig,
    pub events_tx: broadcast::Sender<String>,
}
```

### - [ ] Step 7: Populate `agents_dir` in the test helper

In `mur-core/src/server.rs:1497-1514`, change `test_state` to:

```rust
fn test_state(tmp: &tempfile::TempDir) -> AppState {
    let patterns_dir = tmp.path().join("patterns");
    let workflows_dir = tmp.path().join("workflows");
    let pipelines_dir = tmp.path().join("pipelines");
    let agents_dir = tmp.path().join("agents");
    let index_dir = tmp.path().join("index"); // non-existent → keyword-only fallback
    std::fs::create_dir_all(&patterns_dir).unwrap();
    std::fs::create_dir_all(&workflows_dir).unwrap();
    std::fs::create_dir_all(&pipelines_dir).unwrap();
    std::fs::create_dir_all(&agents_dir).unwrap();
    let (events_tx, _) = broadcast::channel(64);
    AppState {
        patterns_dir,
        workflows_dir,
        pipelines_dir,
        agents_dir,
        index_dir,
        config: ServerConfig { readonly: false },
        events_tx,
    }
}
```

### - [ ] Step 8: Populate `agents_dir` at the real call site

`grep -n "AppState {" mur-core/src/cmd/server_cmd.rs` will show where production `AppState` is built. Add `agents_dir: crate::paths::mur_root(None).join("agents"),` to that struct literal. If `server_cmd.rs` does not directly construct it, the call lives in `mur-core/src/cmd/init.rs` or wherever `mur serve` boots — search with `rg -n "AppState\s*\{" mur-core/src` and add the field everywhere it appears.

### - [ ] Step 9: Mount the nested router

In `mur-core/src/server.rs:142-216`, locate `pub fn build_router(state: AppState) -> Router`. Just before the final `.with_state(...)` call (or wherever the chain currently ends), add:

```rust
        .nest("/api/v1/agents", crate::server_agents::router())
```

### - [ ] Step 10: Run the smoke test plus the full server test suite

Run: `cargo test -p mur-core --features server`

Expected: PASS — the existing tests still pass; the new `test_agents_router_mounted_returns_404_for_unknown_subpath` test passes.

### - [ ] Step 11: Commit

```bash
git add mur-core/src/server.rs mur-core/src/server_agents/ mur-core/src/lib.rs mur-core/src/cmd/
git commit -m "feat(server): scaffold /api/v1/agents nested router (Phase 4 setup)"
```

---

## Task 2: `GET /api/v1/agents` — list

**Files:**
- Modify: `mur-core/src/server_agents/list.rs`
- Modify: `mur-core/src/server_agents/mod.rs` (register route)

### - [ ] Step 1: Write the failing test

Replace the contents of `mur-core/src/server_agents/list.rs` with a test-first stub:

```rust
//! `GET /api/v1/agents` — list agents under `~/.mur/agents/`.

#[cfg(test)]
mod tests {
    use crate::server::build_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn write_min_profile(dir: &std::path::Path, name: &str, display: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let yaml = format!(
            "schema: 1\nid: 0190ad3a-0000-7000-8000-{:012x}\nname: {name}\n\
             display_name: {display}\nversion: \"1.0.0\"\npersona:\n  category: assistant\n  \
             description: t\n  traits:\n    tone: neutral\n    risk: low\n    verbosity: brief\n\
             sys_prompt_file: sys_prompt.md\nmodel:\n  provider: ollama\n  name: qwen3:4b\n  params: {{}}\n\
             mcp_servers: []\nskills: []\ntransport:\n  stdio: true\n  socket:\n    enabled: false\n    \
             bind: \"\"\ncommunication:\n  accepts_from: []\n  sends_to: []\ncapabilities: []\n\
             entitlements:\n  network:\n    inbound:\n      enabled: false\n      ports: []\n    \
             outbound:\n      enabled: false\n      hosts: []\n  filesystem:\n    read_paths: []\n    \
             write_paths: []\n    deny_paths: []\n  processes:\n    spawn: []\n    deny_spawn: []\n  \
             syscalls:\n    deny: []\n  limits:\n    max_memory_mb: 0\n    max_cpu_percent: 0\n    \
             timeout_seconds: 0\nnotifications:\n  on_error: []\nretry:\n  max_attempts: 0\n  \
             backoff_seconds: 0\nlifecycle:\n  auto_restart: false\ncreated_at: \"2026-04-28T00:00:00Z\"\n\
             updated_at: \"2026-04-28T00:00:00Z\"\n",
            name.bytes().fold(0u64, |a, b| a.wrapping_add(b as u64)),
        );
        std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
    }

    #[tokio::test]
    async fn list_returns_known_agents_with_running_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("agents");
        write_min_profile(&agents_dir.join("alpha"), "alpha", "Alpha Bot");
        write_min_profile(&agents_dir.join("beta"), "beta", "Beta Bot");
        // alpha is "running" — touch the lock file
        std::fs::write(agents_dir.join("alpha").join("running.lock"), "1234").unwrap();

        // Build state pointing at this temp agents dir
        let state = crate::server::AppState {
            patterns_dir: tmp.path().join("patterns"),
            workflows_dir: tmp.path().join("workflows"),
            pipelines_dir: tmp.path().join("pipelines"),
            agents_dir,
            index_dir: tmp.path().join("index"),
            config: crate::server::ServerConfig { readonly: false },
            events_tx: tokio::sync::broadcast::channel(64).0,
        };
        for d in [&state.patterns_dir, &state.workflows_dir, &state.pipelines_dir] {
            std::fs::create_dir_all(d).unwrap();
        }

        let app = build_router(state);
        let resp = app
            .oneshot(Request::builder().uri("/api/v1/agents").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().expect("response is an array");
        assert_eq!(arr.len(), 2);

        let alpha = arr.iter().find(|a| a["name"] == "alpha").unwrap();
        assert_eq!(alpha["display_name"], "Alpha Bot");
        assert_eq!(alpha["running"], true);

        let beta = arr.iter().find(|a| a["name"] == "beta").unwrap();
        assert_eq!(beta["running"], false);
    }
}
```

### - [ ] Step 2: Run the test to verify it fails

Run: `cargo test -p mur-core --features server list_returns_known_agents_with_running_flag`

Expected: FAIL — the route doesn't exist, so the response status will be 404, not 200.

### - [ ] Step 3: Implement the handler

Replace `mur-core/src/server_agents/list.rs` with (keeping the existing `#[cfg(test)] mod tests` block at the bottom):

```rust
//! `GET /api/v1/agents` — list agents under `~/.mur/agents/`.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::server::AppState;

#[derive(Serialize)]
pub struct AgentListEntry {
    pub name: String,
    pub display_name: String,
    pub running: bool,
}

pub async fn handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let entries = match std::fs::read_dir(&state.agents_dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::OK, Json(Vec::<AgentListEntry>::new())).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    let mut out: Vec<AgentListEntry> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let profile_path = path.join("profile.yaml");
        let yaml = match std::fs::read_to_string(&profile_path) {
            Ok(y) => y,
            Err(_) => continue, // not an agent dir
        };
        let profile: mur_common::AgentProfile = match serde_yaml_ng::from_str(&yaml) {
            Ok(p) => p,
            Err(_) => continue,
        };
        out.push(AgentListEntry {
            name: profile.name,
            display_name: profile.display_name,
            running: super::is_running(&path),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));

    (StatusCode::OK, Json(out)).into_response()
}

// ── tests block from Step 1 stays here unchanged ──
```

### - [ ] Step 4: Register the route

In `mur-core/src/server_agents/mod.rs`, change `router()` to:

```rust
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", axum::routing::get(list::handler))
}
```

### - [ ] Step 5: Run the test to verify it passes

Run: `cargo test -p mur-core --features server list_returns_known_agents_with_running_flag`

Expected: PASS — both agents listed, alpha shows `running: true`, beta shows `running: false`.

### - [ ] Step 6: Commit

```bash
git add mur-core/src/server_agents/list.rs mur-core/src/server_agents/mod.rs
git commit -m "feat(server): GET /api/v1/agents — list with running status"
```

---

## Task 3: `GET /api/v1/agents/{name}` — detail

**Files:**
- Modify: `mur-core/src/server_agents/detail.rs`
- Modify: `mur-core/src/server_agents/mod.rs` (register route)

### - [ ] Step 1: Write the failing test

Replace `mur-core/src/server_agents/detail.rs` with:

```rust
//! `GET /api/v1/agents/{name}` — full profile + status.

#[cfg(test)]
mod tests {
    use crate::server::{AppState, ServerConfig, build_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn build_state(tmp: &tempfile::TempDir) -> AppState {
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        AppState {
            patterns_dir: tmp.path().join("patterns"),
            workflows_dir: tmp.path().join("workflows"),
            pipelines_dir: tmp.path().join("pipelines"),
            agents_dir,
            index_dir: tmp.path().join("index"),
            config: ServerConfig { readonly: false },
            events_tx: tokio::sync::broadcast::channel(64).0,
        }
    }

    #[tokio::test]
    async fn detail_returns_full_profile_and_status() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha Bot");
        std::fs::write(home.join("running.lock"), "9999").unwrap();

        let app = build_router(state);
        let resp = app
            .oneshot(Request::builder().uri("/api/v1/agents/alpha").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["profile"]["name"], "alpha");
        assert_eq!(json["profile"]["display_name"], "Alpha Bot");
        assert_eq!(json["profile"]["model"]["provider"], "ollama");
        assert_eq!(json["status"]["running"], true);
        assert_eq!(json["status"]["pid"], 9999);
    }

    #[tokio::test]
    async fn detail_returns_404_for_unknown_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let app = build_router(build_state(&tmp));
        let resp = app
            .oneshot(Request::builder().uri("/api/v1/agents/ghost").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
```

This depends on the helper `write_min_profile` from Task 2 being `pub(crate)` — make it so by changing `fn write_min_profile` to `pub(crate) fn write_min_profile` in `list.rs`'s test module, and also annotate the test module itself: `#[cfg(test)] pub(crate) mod tests`.

### - [ ] Step 2: Run the test to verify it fails

Run: `cargo test -p mur-core --features server detail_returns_full_profile`

Expected: FAIL — route returns 404 because the handler is not registered.

### - [ ] Step 3: Implement the handler

Replace `mur-core/src/server_agents/detail.rs` with (keeping the test block):

```rust
//! `GET /api/v1/agents/{name}` — full profile + status.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::server::AppState;

#[derive(Serialize)]
pub struct AgentDetail {
    pub profile: mur_common::AgentProfile,
    pub status: AgentStatus,
}

#[derive(Serialize)]
pub struct AgentStatus {
    pub running: bool,
    pub pid: Option<u32>,
}

pub async fn handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let home = super::agent_home(&state.agents_dir, &name);
    let profile_path = home.join("profile.yaml");
    let yaml = match std::fs::read_to_string(&profile_path) {
        Ok(y) => y,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };
    let profile: mur_common::AgentProfile = match serde_yaml_ng::from_str(&yaml) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("parse profile.yaml: {e}"))
                .into_response();
        }
    };

    let lock_path = home.join("running.lock");
    let pid = std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());
    let running = lock_path.exists();

    (
        StatusCode::OK,
        Json(AgentDetail {
            profile,
            status: AgentStatus { running, pid },
        }),
    )
        .into_response()
}

// ── tests block from Step 1 stays here unchanged ──
```

### - [ ] Step 4: Register the route

In `mur-core/src/server_agents/mod.rs`, change `router()` to:

```rust
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
}
```

### - [ ] Step 5: Run the test to verify it passes

Run: `cargo test -p mur-core --features server detail_returns`

Expected: PASS — both `detail_returns_full_profile_and_status` and `detail_returns_404_for_unknown_agent`.

### - [ ] Step 6: Commit

```bash
git add mur-core/src/server_agents/detail.rs mur-core/src/server_agents/mod.rs mur-core/src/server_agents/list.rs
git commit -m "feat(server): GET /api/v1/agents/{name} — profile + lock-file status"
```

---

## Task 4: `GET /api/v1/agents/{name}/telemetry` — JSONL tail

**Files:**
- Modify: `mur-core/src/server_agents/telemetry.rs`
- Modify: `mur-core/src/server_agents/mod.rs` (register route)

### - [ ] Step 1: Write the failing test

Replace `mur-core/src/server_agents/telemetry.rs` with:

```rust
//! `GET /api/v1/agents/{name}/telemetry` and `WS /api/v1/agents/{name}/stream`.

#[cfg(test)]
mod tests {
    use crate::server::{AppState, ServerConfig, build_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn build_state(tmp: &tempfile::TempDir) -> AppState {
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        AppState {
            patterns_dir: tmp.path().join("patterns"),
            workflows_dir: tmp.path().join("workflows"),
            pipelines_dir: tmp.path().join("pipelines"),
            agents_dir,
            index_dir: tmp.path().join("index"),
            config: ServerConfig { readonly: false },
            events_tx: tokio::sync::broadcast::channel(64).0,
        }
    }

    fn write_telemetry(home: &std::path::Path, date: &str, lines: &[&str]) {
        let dir = home.join("telemetry");
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.join("\n") + "\n";
        std::fs::write(dir.join(format!("{date}.jsonl")), body).unwrap();
    }

    #[tokio::test]
    async fn telemetry_returns_recent_lines_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_telemetry(
            &home,
            "2026-04-28",
            &[
                r#"{"ts":"2026-04-28T07:00:00Z","kind":"start"}"#,
                r#"{"ts":"2026-04-28T07:00:01Z","kind":"message_send"}"#,
                r#"{"ts":"2026-04-28T07:00:02Z","kind":"message_done"}"#,
            ],
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=2026-04-28&limit=10")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["kind"], "start");
        assert_eq!(arr[2]["kind"], "message_done");
    }

    #[tokio::test]
    async fn telemetry_filters_by_since_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_telemetry(
            &home,
            "2026-04-28",
            &[
                r#"{"ts":"2026-04-28T07:00:00Z","kind":"start"}"#,
                r#"{"ts":"2026-04-28T07:00:05Z","kind":"message_send"}"#,
                r#"{"ts":"2026-04-28T07:00:10Z","kind":"message_done"}"#,
            ],
        );

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=2026-04-28&since=2026-04-28T07:00:04Z")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2); // dropped the 07:00:00 line
        assert_eq!(arr[0]["kind"], "message_send");
    }

    #[tokio::test]
    async fn telemetry_returns_empty_when_no_file_for_date() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/telemetry?date=1999-01-01")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }
}
```

### - [ ] Step 2: Run the tests to verify they fail

Run: `cargo test -p mur-core --features server telemetry_`

Expected: FAIL — three tests, all returning 404 because the route is not registered.

### - [ ] Step 3: Implement the handler

Add to `mur-core/src/server_agents/telemetry.rs` (above the test module):

```rust
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::server::AppState;

#[derive(Deserialize)]
pub struct TelemetryQuery {
    /// `YYYY-MM-DD`. Defaults to today (UTC).
    pub date: Option<String>,
    /// Drop lines whose `ts` field is `<= since` (RFC 3339 string compare —
    /// telemetry timestamps are always UTC ISO-8601 so lex order = time order).
    pub since: Option<String>,
    /// Cap returned lines. Default: no cap.
    pub limit: Option<usize>,
}

pub async fn tail_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(q): Query<TelemetryQuery>,
) -> impl IntoResponse {
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }
    let date = q.date.unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    let path = home.join("telemetry").join(format!("{date}.jsonl"));
    let body = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut out: Vec<serde_json::Value> = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue, // skip malformed lines
        };
        if let Some(ref since) = q.since
            && let Some(ts) = v.get("ts").and_then(|t| t.as_str())
            && ts.as_bytes() <= since.as_bytes()
        {
            continue;
        }
        out.push(v);
    }
    if let Some(n) = q.limit
        && out.len() > n
    {
        let drop = out.len() - n;
        out.drain(0..drop);
    }

    (StatusCode::OK, Json(out)).into_response()
}
```

### - [ ] Step 4: Register the route

Update `mur-core/src/server_agents/mod.rs` `router()`:

```rust
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
        .route("/{name}/telemetry", axum::routing::get(telemetry::tail_handler))
}
```

### - [ ] Step 5: Run the tests to verify they pass

Run: `cargo test -p mur-core --features server telemetry_`

Expected: PASS — three tests pass: returns lines in order, filters by `since`, returns empty for missing date.

### - [ ] Step 6: Commit

```bash
git add mur-core/src/server_agents/telemetry.rs mur-core/src/server_agents/mod.rs
git commit -m "feat(server): GET /api/v1/agents/{name}/telemetry — JSONL tail with date+since+limit"
```

---

## Task 5: `WS /api/v1/agents/{name}/stream` — live telemetry tail

**Files:**
- Modify: `mur-core/src/server_agents/telemetry.rs`
- Modify: `mur-core/src/server_agents/mod.rs` (register route)

### - [ ] Step 1: Write the failing test

Append to the `mod tests` block at the bottom of `mur-core/src/server_agents/telemetry.rs`:

```rust
    #[tokio::test]
    async fn ws_stream_emits_lines_appended_after_connect() {
        use axum::extract::ws::Message;
        use tokio_tungstenite::tungstenite;

        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        std::fs::create_dir_all(home.join("telemetry")).unwrap();
        let log_path = home.join("telemetry").join(format!("{date}.jsonl"));
        std::fs::write(&log_path, "").unwrap();

        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("ws://127.0.0.1:{port}/api/v1/agents/alpha/stream");
        let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // Append after the WS is connected
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        std::fs::write(
            &log_path,
            format!(
                r#"{{"ts":"{date}T00:00:00Z","kind":"hello"}}
"#
            ),
        )
        .unwrap();

        // Read one frame within a generous timeout (poll interval is 250ms)
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), {
            use futures_util::StreamExt;
            async { ws.next().await }
        })
        .await
        .expect("ws frame within 3s")
        .expect("stream not closed")
        .expect("frame ok");

        match msg {
            tungstenite::Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                assert_eq!(v["kind"], "hello");
            }
            other => panic!("expected text frame, got {other:?}"),
        }
    }
```

This test pulls in `tokio-tungstenite` and `futures-util`. Add them under `[dev-dependencies]` in `mur-core/Cargo.toml`:

```toml
tokio-tungstenite = { version = "0.24", default-features = false, features = ["connect", "handshake"] }
futures-util = "0.3"
```

(If those crates are already present at the workspace level, just add `tokio-tungstenite = { workspace = true }` etc.)

### - [ ] Step 2: Run the test to verify it fails

Run: `cargo test -p mur-core --features server ws_stream_emits_lines_appended`

Expected: FAIL — handshake fails because the route isn't registered. The test panics inside `connect_async`.

### - [ ] Step 3: Implement the handler

Append to the implementation section of `mur-core/src/server_agents/telemetry.rs` (above the `#[cfg(test)]` block):

```rust
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use std::io::SeekFrom;
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::time::{Duration, sleep};

const POLL_INTERVAL_MS: u64 = 250;

pub async fn stream_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }
    ws.on_upgrade(move |socket| stream_loop(socket, home))
}

async fn stream_loop(mut socket: WebSocket, home: std::path::PathBuf) {
    // Always tail today's file (UTC). On midnight rollover the loop
    // re-resolves the path on the next tick.
    let mut current_date = String::new();
    let mut pos: u64 = 0;
    let mut leftover = String::new();

    loop {
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        if date != current_date {
            current_date = date.clone();
            pos = 0;
            leftover.clear();
            // Start at end-of-file so we only emit *new* lines after connect.
            let path = home.join("telemetry").join(format!("{date}.jsonl"));
            if let Ok(meta) = tokio::fs::metadata(&path).await {
                pos = meta.len();
            }
        }

        let path = home.join("telemetry").join(format!("{current_date}.jsonl"));
        if let Ok(mut f) = tokio::fs::File::open(&path).await {
            let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
            if len < pos {
                // truncated/rotated under us — restart from byte 0
                pos = 0;
                leftover.clear();
            }
            if len > pos {
                if f.seek(SeekFrom::Start(pos)).await.is_ok() {
                    let mut buf = String::new();
                    if BufReader::new(&mut f).read_to_string(&mut buf).await.is_ok() {
                        pos = len;
                        leftover.push_str(&buf);
                        while let Some(idx) = leftover.find('\n') {
                            let line: String = leftover.drain(..=idx).collect();
                            let line = line.trim_end_matches(['\r', '\n']);
                            if line.is_empty() {
                                continue;
                            }
                            if socket.send(Message::Text(line.to_string().into())).await.is_err() {
                                return; // client disconnected
                            }
                        }
                    }
                }
            }
        }

        sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}
```

### - [ ] Step 4: Register the route

Update `mur-core/src/server_agents/mod.rs` `router()`:

```rust
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
        .route("/{name}/telemetry", axum::routing::get(telemetry::tail_handler))
        .route("/{name}/stream", axum::routing::get(telemetry::stream_handler))
}
```

### - [ ] Step 5: Run the test to verify it passes

Run: `cargo test -p mur-core --features server ws_stream_emits_lines_appended -- --nocapture`

Expected: PASS — the WS frame for `kind: "hello"` arrives within 3s.

### - [ ] Step 6: Commit

```bash
git add mur-core/src/server_agents/telemetry.rs mur-core/src/server_agents/mod.rs mur-core/Cargo.toml
git commit -m "feat(server): WS /api/v1/agents/{name}/stream — poll-based JSONL tail"
```

---

## Task 6: `GET /api/v1/agents/{name}/evals` — list runs

**Files:**
- Modify: `mur-core/src/server_agents/evals.rs`
- Modify: `mur-core/src/server_agents/mod.rs` (register route)

### - [ ] Step 1: Write the failing test

Replace `mur-core/src/server_agents/evals.rs` with:

```rust
//! `GET /api/v1/agents/{name}/evals[/{run_id}]`.

#[cfg(test)]
mod tests {
    use crate::server::{AppState, ServerConfig, build_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    pub(crate) fn build_state(tmp: &tempfile::TempDir) -> AppState {
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        AppState {
            patterns_dir: tmp.path().join("patterns"),
            workflows_dir: tmp.path().join("workflows"),
            pipelines_dir: tmp.path().join("pipelines"),
            agents_dir,
            index_dir: tmp.path().join("index"),
            config: ServerConfig { readonly: false },
            events_tx: tokio::sync::broadcast::channel(64).0,
        }
    }

    pub(crate) fn write_eval(home: &std::path::Path, run_id: &str, suite: &str, started_at: &str) {
        let dir = home.join("evals");
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "run_id": run_id,
            "suite": suite,
            "agent": "alpha",
            "started_at": started_at,
            "finished_at": started_at,
            "matrix": {"providers": [], "system_prompts": []},
            "results": [],
            "summary": {"total": 0, "passed": 0, "failed": 0}
        });
        std::fs::write(
            dir.join(format!("{run_id}.json")),
            serde_json::to_vec_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn list_evals_returns_run_summaries_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_eval(&home, "eval-20260427T100000-aaaa", "v1", "2026-04-27T10:00:00Z");
        write_eval(&home, "eval-20260428T072501-bbbb", "v1", "2026-04-28T07:25:01Z");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder().uri("/api/v1/agents/alpha/evals").body(Body::empty()).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["run_id"], "eval-20260428T072501-bbbb"); // newest first
        assert_eq!(arr[1]["run_id"], "eval-20260427T100000-aaaa");
        assert_eq!(arr[0]["suite"], "v1");
    }

    #[tokio::test]
    async fn list_evals_returns_empty_when_no_evals_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder().uri("/api/v1/agents/alpha/evals").body(Body::empty()).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json, serde_json::json!([]));
    }
}
```

### - [ ] Step 2: Run the tests to verify they fail

Run: `cargo test -p mur-core --features server list_evals_`

Expected: FAIL — both tests get 404 because the route is not registered.

### - [ ] Step 3: Implement the handler

Add to `mur-core/src/server_agents/evals.rs` (above the test module):

```rust
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::server::AppState;

#[derive(Serialize)]
pub struct EvalRunSummary {
    pub run_id: String,
    pub suite: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub summary: serde_json::Value,
}

pub async fn list_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let home = super::agent_home(&state.agents_dir, &name);
    if !home.exists() {
        return (StatusCode::NOT_FOUND, format!("agent '{name}' not found")).into_response();
    }
    let dir = home.join("evals");
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::OK, Json(Vec::<EvalRunSummary>::new())).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let mut out: Vec<EvalRunSummary> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let run_id = v.get("run_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
        if run_id.is_empty() {
            continue;
        }
        let suite = v.get("suite").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let started_at = v.get("started_at").and_then(|s| s.as_str()).map(String::from);
        let finished_at = v.get("finished_at").and_then(|s| s.as_str()).map(String::from);
        let summary = v.get("summary").cloned().unwrap_or(serde_json::json!({}));
        out.push(EvalRunSummary { run_id, suite, started_at, finished_at, summary });
    }
    // run_id starts with `eval-<TIMESTAMP>-<HASH>`; lex sort = chronological.
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));

    (StatusCode::OK, Json(out)).into_response()
}
```

### - [ ] Step 4: Register the route

Update `mur-core/src/server_agents/mod.rs` `router()`:

```rust
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
        .route("/{name}/telemetry", axum::routing::get(telemetry::tail_handler))
        .route("/{name}/stream", axum::routing::get(telemetry::stream_handler))
        .route("/{name}/evals", axum::routing::get(evals::list_handler))
}
```

### - [ ] Step 5: Run the tests to verify they pass

Run: `cargo test -p mur-core --features server list_evals_`

Expected: PASS — both tests pass.

### - [ ] Step 6: Commit

```bash
git add mur-core/src/server_agents/evals.rs mur-core/src/server_agents/mod.rs
git commit -m "feat(server): GET /api/v1/agents/{name}/evals — newest-first run summaries"
```

---

## Task 7: `GET /api/v1/agents/{name}/evals/{run_id}` — full run

**Files:**
- Modify: `mur-core/src/server_agents/evals.rs`
- Modify: `mur-core/src/server_agents/mod.rs` (register route)

### - [ ] Step 1: Write the failing test

Append to the `mod tests` block at the bottom of `mur-core/src/server_agents/evals.rs`:

```rust
    #[tokio::test]
    async fn detail_eval_returns_full_run_json() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");
        write_eval(&home, "eval-20260428T072501-bbbb", "v1", "2026-04-28T07:25:01Z");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals/eval-20260428T072501-bbbb")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["run_id"], "eval-20260428T072501-bbbb");
        assert_eq!(json["suite"], "v1");
        assert!(json.get("results").is_some());
        assert!(json.get("summary").is_some());
    }

    #[tokio::test]
    async fn detail_eval_returns_404_for_unknown_run() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals/ghost-run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn detail_eval_rejects_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let state = build_state(&tmp);
        let home = state.agents_dir.join("alpha");
        super::super::list::tests::write_min_profile(&home, "alpha", "Alpha");

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/agents/alpha/evals/..%2F..%2Fetc%2Fpasswd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
```

### - [ ] Step 2: Run the tests to verify they fail

Run: `cargo test -p mur-core --features server detail_eval_`

Expected: FAIL — three tests, all 404 because the route isn't registered.

### - [ ] Step 3: Implement the handler

Add to the implementation section of `mur-core/src/server_agents/evals.rs` (above the test module):

```rust
pub async fn detail_handler(
    State(state): State<Arc<AppState>>,
    Path((name, run_id)): Path<(String, String)>,
) -> impl IntoResponse {
    // run_id is used as a filename — reject anything that could escape the
    // evals/ directory (path separators, '..', null bytes, etc.).
    let safe = !run_id.is_empty()
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !safe || run_id.contains("..") {
        return (StatusCode::BAD_REQUEST, "invalid run_id".to_string()).into_response();
    }

    let home = super::agent_home(&state.agents_dir, &name);
    let path = home.join("evals").join(format!("{run_id}.json"));
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (StatusCode::NOT_FOUND, format!("eval '{run_id}' not found")).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("parse eval json: {e}"))
                .into_response();
        }
    };

    (StatusCode::OK, Json(v)).into_response()
}
```

### - [ ] Step 4: Register the route

Update `mur-core/src/server_agents/mod.rs` `router()`:

```rust
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/", axum::routing::get(list::handler))
        .route("/{name}", axum::routing::get(detail::handler))
        .route("/{name}/telemetry", axum::routing::get(telemetry::tail_handler))
        .route("/{name}/stream", axum::routing::get(telemetry::stream_handler))
        .route("/{name}/evals", axum::routing::get(evals::list_handler))
        .route("/{name}/evals/{run_id}", axum::routing::get(evals::detail_handler))
}
```

### - [ ] Step 5: Run the tests to verify they pass

Run: `cargo test -p mur-core --features server detail_eval_`

Expected: PASS — three tests pass: full body returned, 404 for unknown, 400 for traversal attempt.

### - [ ] Step 6: Commit

```bash
git add mur-core/src/server_agents/evals.rs mur-core/src/server_agents/mod.rs
git commit -m "feat(server): GET /api/v1/agents/{name}/evals/{run_id} — full run JSON"
```

---

## Task 8: End-to-end smoke test + clippy + fmt + PR

**Files:**
- Create: `mur-core/tests/server_agents_routes.rs`

### - [ ] Step 1: Write an integration smoke test

Create `mur-core/tests/server_agents_routes.rs`:

```rust
//! End-to-end smoke for the read-only Phase 4 agent routes.
//! Spins up the real `build_router` against a tempdir, exercises every route.

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use mur_core::server::{AppState, ServerConfig, build_router};

fn build_state(tmp: &tempfile::TempDir) -> AppState {
    let agents_dir = tmp.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    AppState {
        patterns_dir: tmp.path().join("patterns"),
        workflows_dir: tmp.path().join("workflows"),
        pipelines_dir: tmp.path().join("pipelines"),
        agents_dir,
        index_dir: tmp.path().join("index"),
        config: ServerConfig { readonly: false },
        events_tx: tokio::sync::broadcast::channel(64).0,
    }
}

fn write_min_profile(dir: &std::path::Path, name: &str, display: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let yaml = format!(
        "schema: 1\nid: 0190ad3a-0000-7000-8000-000000000001\nname: {name}\n\
         display_name: {display}\nversion: \"1.0.0\"\npersona:\n  category: assistant\n  \
         description: t\n  traits:\n    tone: neutral\n    risk: low\n    verbosity: brief\n\
         sys_prompt_file: sys_prompt.md\nmodel:\n  provider: ollama\n  name: qwen3:4b\n  params: {{}}\n\
         mcp_servers: []\nskills: []\ntransport:\n  stdio: true\n  socket:\n    enabled: false\n    \
         bind: \"\"\ncommunication:\n  accepts_from: []\n  sends_to: []\ncapabilities: []\n\
         entitlements:\n  network:\n    inbound:\n      enabled: false\n      ports: []\n    \
         outbound:\n      enabled: false\n      hosts: []\n  filesystem:\n    read_paths: []\n    \
         write_paths: []\n    deny_paths: []\n  processes:\n    spawn: []\n    deny_spawn: []\n  \
         syscalls:\n    deny: []\n  limits:\n    max_memory_mb: 0\n    max_cpu_percent: 0\n    \
         timeout_seconds: 0\nnotifications:\n  on_error: []\nretry:\n  max_attempts: 0\n  \
         backoff_seconds: 0\nlifecycle:\n  auto_restart: false\ncreated_at: \"2026-04-28T00:00:00Z\"\n\
         updated_at: \"2026-04-28T00:00:00Z\"\n"
    );
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
}

#[tokio::test]
async fn end_to_end_all_readonly_routes() {
    let tmp = tempfile::tempdir().unwrap();
    let state = build_state(&tmp);
    let home = state.agents_dir.join("support_bot");
    write_min_profile(&home, "support_bot", "Support");
    std::fs::create_dir_all(home.join("telemetry")).unwrap();
    std::fs::write(
        home.join("telemetry").join("2026-04-28.jsonl"),
        r#"{"ts":"2026-04-28T07:00:00Z","kind":"start"}
"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.join("evals")).unwrap();
    std::fs::write(
        home.join("evals").join("eval-20260428T072501-aaaa.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "run_id": "eval-20260428T072501-aaaa",
            "suite": "v1",
            "agent": "support_bot",
            "started_at": "2026-04-28T07:25:01Z",
            "finished_at": "2026-04-28T07:25:05Z",
            "matrix": {"providers": ["ollama/llama3.2:3b"], "system_prompts": ["a"]},
            "results": [],
            "summary": {"total": 0, "passed": 0, "failed": 0}
        }))
        .unwrap(),
    )
    .unwrap();

    let app = build_router(state);
    for path in [
        "/api/v1/agents",
        "/api/v1/agents/support_bot",
        "/api/v1/agents/support_bot/telemetry?date=2026-04-28",
        "/api/v1/agents/support_bot/evals",
        "/api/v1/agents/support_bot/evals/eval-20260428T072501-aaaa",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "route {path} returned non-200");
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(!bytes.is_empty(), "route {path} returned empty body");
    }
}
```

### - [ ] Step 2: Run the smoke test

Run: `cargo test -p mur-core --features server --test server_agents_routes`

Expected: PASS.

### - [ ] Step 3: Run the full mur-core test suite to catch regressions

Run: `cargo test -p mur-core --features server`

Expected: PASS — no existing tests broken.

### - [ ] Step 4: Run clippy and fmt

Run:
```bash
cargo clippy --workspace --features server -- -D warnings
cargo fmt --check
```

Expected: clean. Fix any warnings before continuing.

### - [ ] Step 5: Commit

```bash
git add mur-core/tests/server_agents_routes.rs
git commit -m "test(server): end-to-end smoke for /api/v1/agents/* read routes"
```

### - [ ] Step 6: Push and open the PR

```bash
git push -u origin feat/serve-agents-routes
gh pr create --title "feat(server): /api/v1/agents/* read-only routes (Phase 4)" --body "$(cat <<'EOF'
## Summary
- Adds nested `/api/v1/agents/*` router with 6 read-only endpoints.
- Backed by `~/.mur/agents/<name>/{profile.yaml, running.lock, telemetry/<date>.jsonl, evals/<id>.json}` — no daemon, no socket calls.
- Live telemetry tail via WebSocket (poll-based, 250ms tick).
- Implements Phase 4 of the design at `mur-agent-harness/phase9/design.md` ("mur serve agent routes" table — read-only rows only).
- Recovers work from the 2026-04-28 reboot incident where `/private/tmp/mur-feat-serve` was wiped.

## Routes
| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/v1/agents` | List agents under `~/.mur/agents/` with running flag |
| GET | `/api/v1/agents/{name}` | Full profile + lock-file status (running/pid) |
| GET | `/api/v1/agents/{name}/telemetry?date&since&limit` | JSONL tail |
| WS  | `/api/v1/agents/{name}/stream` | Live tail of today's JSONL |
| GET | `/api/v1/agents/{name}/evals` | Run summaries, newest first |
| GET | `/api/v1/agents/{name}/evals/{run_id}` | Full run JSON |

## Out of scope (Phase 6)
- Write actions (POST messages, PUT profile, POST spawn/stop/run).
- Auth / CSRF.
- SPA dashboard tab (Phase 5).

## Test plan
- [x] Per-handler unit tests in each `server_agents/<name>.rs`
- [x] WebSocket round-trip test in `telemetry.rs`
- [x] End-to-end smoke in `tests/server_agents_routes.rs`
- [x] `cargo clippy --workspace --features server -- -D warnings`
- [x] `cargo fmt --check`
EOF
)"
```

---

## Self-Review

### Spec coverage (against `phase9/design.md` lines 154-170)

| Design row | Plan task |
|------------|-----------|
| `GET /api/v1/agents` | Task 2 |
| `GET /api/v1/agents/{name}` | Task 3 |
| `GET /api/v1/agents/{name}/telemetry?since=ts` | Task 4 (adds `date` and `limit` for safety) |
| `WS /api/v1/agents/{name}/stream` | Task 5 |
| `GET /api/v1/agents/{name}/evals` | Task 6 |
| `GET /api/v1/agents/{name}/evals/{run_id}` | Task 7 |
| `POST /api/v1/agents/{name}/messages` | **out of scope** — Phase 6 (write) |
| `POST /api/v1/agents/{name}/evals/run` | **out of scope** — Phase 6 (write) |
| `PUT /api/v1/agents/{name}/profile` | **out of scope** — Phase 6 (write) |
| `POST /api/v1/agents/{name}/spawn` | **out of scope** — Phase 6 (write, admin) |
| `POST /api/v1/agents/{name}/stop` | **out of scope** — Phase 6 (write, admin) |

All read-only rows are covered. Write rows are explicitly deferred to Phase 6 in the PR body.

### Type consistency

- `AppState.agents_dir: PathBuf` — added in Task 1, consumed by every handler in Tasks 2–7.
- `AgentListEntry` (Task 2), `AgentDetail` + `AgentStatus` (Task 3), `EvalRunSummary` (Task 6), `TelemetryQuery` (Task 4) — each lives in its handler file and is named consistently with its module.
- `super::agent_home(...)` and `super::is_running(...)` — defined once in `mod.rs` (Task 1), called from `detail`, `telemetry`, `evals`.
- Router method names are stable across tasks: `list::handler`, `detail::handler`, `telemetry::tail_handler`, `telemetry::stream_handler`, `evals::list_handler`, `evals::detail_handler`.

### Placeholder scan

Each step contains executable code or shell commands; no "TBD" / "implement appropriate handler" / "add error handling" placeholders. The one cross-task call (`super::super::list::tests::write_min_profile`) is real — Task 3 Step 1 explicitly bumps the helper visibility to `pub(crate)`.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-04-28-mur-serve-agents-routes-phase4.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration. Good fit here because each task is independently testable and the worktree is isolated.

**2. Inline Execution** — I execute tasks in this session using `superpowers:executing-plans`, batching checkpoints for review.

**Which approach?**
