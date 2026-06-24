# Agent Runtime Upgrade — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect stale agent runtimes, version-gate the A2A dial so version skew fails with an actionable error (not opaque `-32601`), and graceful-restart agents (drain in-flight work; never auto-bounce).

**Architecture:** Embed a build-id (git sha) + a semantic `A2A_PROTO_VERSION` in `mur-common`; carry both in `running.lock` + the AgentCard. The dial pre-checks the peer's proto and refuses unsupported methods with a `StaleRuntime` error. A `doctor`/`status`/post-build nudge compares build-ids to flag stale agents. SIGTERM drains the in-flight turn before teardown; `mur agent restart` SIGTERMs the pid and lets launchd respawn on the fresh binary.

**Tech Stack:** Rust (edition 2024), `mur-common` (constants + LockFile), `mur-agent-runtime` (supervisor/TaskRunner/card), `mur-core` (`a2a_dial`, `cmd/agent`), launchd `KeepAlive`.

## Global Constraints

- **Rust edition 2024.** No hardcoded behaviour values — version numbers are named consts.
- **mur-core / mur-agent-runtime tests need `ORT_STRATEGY=download`** and run under `cargo nextest`, not `cargo test`. All commands below use it.
- **Back-compat is mandatory:** new `LockFile` fields are `#[serde(default)]` so old locks parse (absent → `""` / `0` = "stale / unsupported", which is the intended reading). `method_min_proto == 0` methods (`message/send`) are NEVER gated.
- **Never auto-restart a running agent.** Notify is print-only. Restart is explicit and graceful (SIGTERM-drain, never SIGKILL-first).
- **Single source file ≤ 800 lines** — new commands go in their own `cmd/agent/{doctor,restart}.rs`.

---

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `mur-common/build.rs` | embed git sha at build time | **new** |
| `mur-common/Cargo.toml` | declare `build = "build.rs"` | modify |
| `mur-common/src/build.rs` + `lib.rs` | `SHORT_SHA`, `A2A_PROTO_VERSION`, `method_min_proto` | **new** module |
| `mur-common/src/agent.rs:834` | `LockFile.build_sha` + `proto_version` | modify |
| `mur-agent-runtime/src/supervisor.rs` | write new lock fields; `--build-id` flag; SIGTERM drain | modify |
| `mur-agent-runtime/src/task_runner.rs` | `drain(timeout)`: stop-accepting + await active | modify |
| `mur-agent-runtime/src/protocol/methods/card.rs:59` | advertise `proto_version` | modify |
| `mur-core/src/a2a_dial.rs` | pre-flight proto gate + `StaleRuntime` error | modify |
| `mur-core/src/cmd/agent/doctor.rs` | stale detection report | **new** |
| `mur-core/src/cmd/agent/restart.rs` | graceful restart command | **new** |
| `mur-core/src/cmd/agent/{mod,lifecycle}.rs` + `cli/{agent,actions}.rs` | wire commands + status marker | modify |
| `build.sh` | post-install stale nudge | modify |

---

## Phase A — Foundation: build-id + proto version (Tasks 1–3)

### Task 1: Embed git sha (`mur-common` build-id)

**Files:**
- Create: `mur-common/build.rs`
- Modify: `mur-common/Cargo.toml` (add `build = "build.rs"` to `[package]`)
- Create: `mur-common/src/build.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod build;`)
- Test: `mur-common/src/build.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `mur_common::build::SHORT_SHA: &str` (12-char git sha, or `"unknown"`).

- [ ] **Step 1: Add the build script**

Create `mur-common/build.rs`:
```rust
use std::process::Command;

fn main() {
    // Re-run when HEAD moves (a new commit changes the sha).
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=MUR_GIT_SHA={sha}");
}
```

- [ ] **Step 2: Declare the build script**

In `mur-common/Cargo.toml`, add to `[package]` (after `version.workspace = true`):
```toml
build = "build.rs"
```

- [ ] **Step 3: Create the build module with a test**

Create `mur-common/src/build.rs`:
```rust
//! Compile-time build identity. `SHORT_SHA` is the git commit the binary was
//! built from (set by build.rs), or "unknown" for git-less builds (crates.io).
//! Used to detect when a running agent's binary differs from the installed one.

/// 12-char git sha of this build, or "unknown".
pub const SHORT_SHA: &str = env!("MUR_GIT_SHA");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sha_is_set() {
        // Either a real 12-char hex sha, or the "unknown" fallback.
        assert!(SHORT_SHA == "unknown" || SHORT_SHA.len() == 12, "got {SHORT_SHA:?}");
    }
}
```

In `mur-common/src/lib.rs`, add alongside the other `pub mod` lines (alphabetical, after `pub mod bundle;`):
```rust
pub mod build;
```

- [ ] **Step 4: Run the test**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common short_sha_is_set`
Expected: **PASS**.

- [ ] **Step 5: Commit**
```bash
git add mur-common/build.rs mur-common/Cargo.toml mur-common/src/build.rs mur-common/src/lib.rs
git commit -m "feat(common): embed git sha as build identity (SHORT_SHA)"
```

---

### Task 2: Proto-version constants

**Files:**
- Modify: `mur-common/src/build.rs`
- Test: `mur-common/src/build.rs`

**Interfaces:**
- Produces: `A2A_PROTO_VERSION: u32` and `fn method_min_proto(method: &str) -> u32`.

- [ ] **Step 1: Write the failing test**

Append to `mur-common/src/build.rs` tests:
```rust
    #[test]
    fn method_min_proto_gates_channel_delegate_only() {
        // channel/delegate requires the proto that introduced it.
        assert_eq!(method_min_proto("channel/delegate"), 1);
        // Always-available methods are ungated (min 0).
        assert_eq!(method_min_proto("message/send"), 0);
        assert_eq!(method_min_proto("agent/card"), 0);
        // The current proto is at least the highest gated method.
        assert!(A2A_PROTO_VERSION >= 1);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common method_min_proto_gates`
Expected: **compile error** — `method_min_proto` / `A2A_PROTO_VERSION` not found.

- [ ] **Step 3: Implement**

Add to `mur-common/src/build.rs` (above the tests):
```rust
/// A2A method-surface version. Bump ONLY on an incompatible change to a dialed
/// method (added method, changed params/result contract). Carried in
/// `running.lock` + the AgentCard; the dial refuses a method whose
/// `method_min_proto` exceeds the peer's advertised proto.
pub const A2A_PROTO_VERSION: u32 = 1;

/// Minimum proto a peer must advertise to be dialed for `method`. `0` = always
/// available (never gated). Add an entry when a method is introduced/changed.
pub fn method_min_proto(method: &str) -> u32 {
    match method {
        "channel/delegate" => 1,
        _ => 0,
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common method_min_proto_gates`
Expected: **PASS**.

- [ ] **Step 5: Commit**
```bash
git add mur-common/src/build.rs
git commit -m "feat(common): A2A_PROTO_VERSION + method_min_proto gate table"
```

---

### Task 3: LockFile + AgentCard carry build_sha & proto_version

**Files:**
- Modify: `mur-common/src/agent.rs:834-846`
- Modify: `mur-agent-runtime/src/supervisor.rs:507-518` (write fields) + add `--build-id` flag
- Modify: `mur-agent-runtime/src/protocol/methods/card.rs:59`
- Test: `mur-common/src/agent.rs`

**Interfaces:**
- Produces: `LockFile.build_sha: String`, `LockFile.proto_version: u32` (both `#[serde(default)]`).
- Consumed by Tasks 4 (dial gate), 8 (doctor).

- [ ] **Step 1: Write the failing test (back-compat default)**

Add to `mur-common/src/agent.rs` `#[cfg(test)]`:
```rust
    #[test]
    fn lockfile_new_fields_default_for_old_locks() {
        // An old lock JSON without build_sha/proto_version must still parse,
        // defaulting to "" / 0 (= "predates this feature → stale/unsupported").
        let old = r#"{"schema":1,"uuid":"u","name":"a","pid":1,"ppid":1,
          "started_at":"t","binary_version":"mur-agent-runtime 2.26.9",
          "transports":{"stdio":true},"card_digest":"d","capabilities":[]}"#;
        let lock: LockFile = serde_json::from_str(old).unwrap();
        assert_eq!(lock.build_sha, "");
        assert_eq!(lock.proto_version, 0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common lockfile_new_fields_default`
Expected: **compile error** — no field `build_sha`.

- [ ] **Step 3: Add the fields**

In `mur-common/src/agent.rs`, in `pub struct LockFile` (after `capabilities: Vec<String>,`):
```rust
    /// Git sha the running binary was built from (mur_common::build::SHORT_SHA).
    /// Empty = an old lock predating this field. Drives stale detection.
    #[serde(default)]
    pub build_sha: String,
    /// A2A method-surface version this runtime supports (A2A_PROTO_VERSION).
    /// 0 = an old lock; the dial gates versioned methods on it.
    #[serde(default)]
    pub proto_version: u32,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common lockfile_new_fields_default`
Expected: **PASS**.

- [ ] **Step 5: Write the fields at runtime startup**

In `mur-agent-runtime/src/supervisor.rs`, the `LockFile { … }` literal (~`:507`) — add the two fields before the closing brace:
```rust
        build_sha: mur_common::build::SHORT_SHA.to_string(),
        proto_version: mur_common::build::A2A_PROTO_VERSION,
```

- [ ] **Step 6: Add the `--build-id` flag**

In `mur-agent-runtime/src/supervisor.rs`, near the existing `--version` handling (`:62` prints `mur-agent-runtime {CARGO_PKG_VERSION}`), add a `--build-id` arm that prints just the sha and exits 0:
```rust
        if arg == "--build-id" {
            println!("{}", mur_common::build::SHORT_SHA);
            return Ok(());
        }
```
(Match the surrounding arg-parse style; `return Ok(())` consistent with the `--version` early return.)

- [ ] **Step 7: Advertise proto_version on the card**

In `mur-agent-runtime/src/protocol/methods/card.rs`, the `json!({ … })` card (~`:60`), add after `"protocolVersion": "a2a/0.3",`:
```rust
            "proto_version": mur_common::build::A2A_PROTO_VERSION,
```

- [ ] **Step 8: Verify the runtime crate compiles**

Run: `ORT_STRATEGY=download cargo check -p mur-agent-runtime`
Expected: clean.

- [ ] **Step 9: Commit**
```bash
git add mur-common/src/agent.rs mur-agent-runtime/src/supervisor.rs mur-agent-runtime/src/protocol/methods/card.rs
git commit -m "feat(runtime): stamp build_sha + proto_version into lock + card; --build-id"
```

---

## Phase B — ① Dial version-gate (Task 4)

### Task 4: Pre-flight proto gate in `a2a_dial`

**Files:**
- Modify: `mur-core/src/a2a_dial.rs` (`dial_method` ~`:68-123`)
- Test: `mur-core/src/a2a_dial.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `mur_common::build::method_min_proto`, `LockFile.proto_version`.
- Produces: a `StaleRuntime`-shaped error from `dial_method` when a running peer's proto is below the method's min.

- [ ] **Step 1: Write the failing test**

Add to `mur-core/src/a2a_dial.rs` `#[cfg(test)]` (use a temp home + a hand-written stale lock; no socket needed — the gate fires before any dial):
```rust
    #[test]
    fn dial_gates_channel_delegate_on_stale_proto() {
        let tmp = tempfile::TempDir::new().unwrap();
        let adir = tmp.path().join("agents").join("rustsmith");
        std::fs::create_dir_all(&adir).unwrap();
        // Old lock: proto_version absent → 0 < channel/delegate's min (1).
        std::fs::write(adir.join("running.lock"), r#"{"schema":1,"uuid":"u",
          "name":"rustsmith","pid":1,"ppid":1,"started_at":"t",
          "binary_version":"old","transports":{"stdio":true,
          "unix_socket":"/nonexistent.sock"},"card_digest":"d","capabilities":[]}"#).unwrap();

        let err = dial_method(tmp.path(), "rustsmith", "channel/delegate",
            serde_json::json!({}), DialMode::RequireRunning).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("stale runtime"), "got: {msg}");
        assert!(msg.contains("mur agent restart rustsmith"), "got: {msg}");
        // It must NOT have tried to connect to the (nonexistent) socket.
        assert!(!msg.contains("connect"), "gate should fire before dialing: {msg}");
    }

    #[test]
    fn dial_does_not_gate_ungated_method() {
        // message/send (min 0) on the same stale lock must NOT be proto-gated
        // (it will fail later for a different reason — socket — which is fine).
        let tmp = tempfile::TempDir::new().unwrap();
        let adir = tmp.path().join("agents").join("a");
        std::fs::create_dir_all(&adir).unwrap();
        std::fs::write(adir.join("running.lock"), r#"{"schema":1,"uuid":"u","name":"a",
          "pid":1,"ppid":1,"started_at":"t","binary_version":"old",
          "transports":{"stdio":true,"unix_socket":"/nonexistent.sock"},
          "card_digest":"d","capabilities":[]}"#).unwrap();
        let err = dial_method(tmp.path(), "a", "message/send",
            serde_json::json!({}), DialMode::RequireRunning).unwrap_err();
        assert!(!err.to_string().contains("stale runtime"), "must not gate message/send");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core dial_gates_channel_delegate`
Expected: **FAIL** — no stale-runtime gating; the call proceeds to socket-connect.

- [ ] **Step 3: Implement the gate**

In `mur-core/src/a2a_dial.rs`, in `dial_method`, AFTER `let is_running = lock_path.exists();` (~`:92`) and BEFORE the `match (mode, is_running)` block, add the pre-flight gate for running peers:
```rust
    // Pre-flight version gate: refuse a versioned method against a running peer
    // whose advertised proto is too low — with an actionable error, not -32601.
    // Reads the peer's running.lock (cheap, local). Ungated methods (min 0) skip.
    if is_running {
        let needed = mur_common::build::method_min_proto(method);
        if needed > 0 {
            if let Ok(bytes) = fs::read(&lock_path) {
                if let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes) {
                    if lock.proto_version < needed {
                        let sha = if lock.build_sha.is_empty() { "unknown" } else { &lock.build_sha };
                        bail!(
                            "agent '{agent_name}' is running a stale runtime (proto {}, build {}); \
                             the requested capability '{method}' needs proto {needed}. \
                             Run 'mur agent restart {agent_name}' to apply the installed runtime.",
                            lock.proto_version, sha
                        );
                    }
                }
            }
        }
    }
```
(`LockFile`, `fs`, `bail` are already imported in this file.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core dial_gates_channel_delegate dial_does_not_gate`
Expected: **PASS** both.

- [ ] **Step 5: Regression — existing dial tests**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core a2a_dial`
Expected: **PASS** (the gate only fires for running peers on versioned methods).

- [ ] **Step 6: Commit**
```bash
git add mur-core/src/a2a_dial.rs
git commit -m "feat(a2a): pre-flight proto gate — stale runtime → actionable error not -32601"
```

---

## Phase C — ③ Graceful drain + restart (Tasks 5–6)

### Task 5: TaskRunner drain + SIGTERM integration

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (the `TaskRunner` struct ~`:112` + `run_sync_inner`)
- Modify: `mur-agent-runtime/src/supervisor.rs:647-672` (replace the `:655` TODO)
- Test: `mur-agent-runtime/src/task_runner.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `TaskRunner::begin_drain()` (stop accepting new turns) + `async fn await_idle(&self, timeout: Duration) -> bool` (true = drained, false = timed out). `run_sync`/`run_sync_inner` reject with a transient error once draining.

**Implementation note for the implementer:** read `task_runner.rs` first. `TaskRunner` already tracks live turns via `registry: Arc<Mutex<HashMap<String, TaskState>>>` + `registry_keys: Arc<Mutex<VecDeque<String>>>` (`:114-117`). Add an `Arc<AtomicBool> draining` field (default false). `run_sync_inner` (`:495`) returns a transient failure `TaskOutcome` when `draining` is set, before registering a turn. `await_idle` polls `registry_keys` empty (every ~50ms) up to `timeout`. This is the bounded, cooperative drain — no SIGKILL, no mid-turn abort.

- [ ] **Step 1: Write the failing test**
```rust
    #[tokio::test]
    async fn drain_rejects_new_turns_and_reports_idle() {
        let runner = /* construct a minimal TaskRunner — mirror existing runner tests */;
        // Idle runner drains immediately.
        assert!(runner.await_idle(std::time::Duration::from_millis(200)).await);
        // After begin_drain, a new run_sync is rejected (transient), not executed.
        runner.begin_drain();
        let outcome = runner.run_sync(/* a trivial TaskSpec */).await;
        assert!(matches!(outcome, TaskOutcome::Failed { .. }),
            "draining runner must reject new turns");
    }
```
(Mirror the existing TaskRunner test harness for construction + `TaskSpec`/`TaskOutcome` shapes — read the `#[cfg(test)]` block already in `task_runner.rs`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime drain_rejects_new_turns`
Expected: **compile error** — no `begin_drain`/`await_idle`.

- [ ] **Step 3: Implement `draining` + `begin_drain` + `await_idle` + the run_sync guard**

Add an `draining: Arc<std::sync::atomic::AtomicBool>` field to `TaskRunner` (init `false` in its constructor). Add:
```rust
    pub fn begin_drain(&self) {
        self.draining.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Wait until no turns are in flight, bounded by `timeout`. true = drained.
    pub async fn await_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.registry_keys.lock().await.is_empty() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
```
At the TOP of `run_sync_inner` (`:495`), before any turn registration, reject when draining:
```rust
        if self.draining.load(std::sync::atomic::Ordering::SeqCst) {
            return TaskOutcome::Failed { /* transient: "agent draining, retry" — match TaskOutcome::Failed's fields */ };
        }
```
(Use the exact `TaskOutcome::Failed` field shape from the enum definition; message: `"agent is draining for restart; retry shortly"`.)

> The `registry_keys` lock type: confirm it's `tokio::sync::Mutex` (async `.lock().await`) vs `std::sync::Mutex`; `:117` shows the field — match it. If it's `std::sync::Mutex`, drop the `.await` and use `try_lock`/`lock().unwrap()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime drain_rejects_new_turns`
Expected: **PASS**.

- [ ] **Step 5: Wire the drain into SIGTERM shutdown**

In `mur-agent-runtime/src/supervisor.rs`, replace the TODO block (`:655-656`) — BEFORE `for t in transport_tasks { t.abort(); }`:
```rust
    // Drain the in-flight turn cooperatively before tearing down transports:
    // stop accepting new dials, then wait for the active turn to finish, bounded
    // by stop_timeout_secs. Never SIGKILL a turn mid-flight.
    runner.begin_drain();
    let drained = runner
        .await_idle(std::time::Duration::from_secs(profile.inner.lifecycle.stop_timeout_secs))
        .await;
    if !drained {
        warn!("drain timed out after {}s; tearing down with a turn still in flight",
            profile.inner.lifecycle.stop_timeout_secs);
    }
```
(`runner` is the `TaskRunner` Arc already in scope in this function; `warn!` is imported.)

- [ ] **Step 6: Compile + full runtime test sweep**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime task_runner`
Expected: **PASS** (drain tests + existing runner tests).

- [ ] **Step 7: Commit**
```bash
git add mur-agent-runtime/src/task_runner.rs mur-agent-runtime/src/supervisor.rs
git commit -m "feat(runtime): graceful drain — SIGTERM awaits the in-flight turn (closes P0b TODO)"
```

---

### Task 6: `mur agent restart` command

**Files:**
- Create: `mur-core/src/cmd/agent/restart.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs` (declare `mod restart;` + re-export)
- Modify: `mur-core/src/cli/agent.rs` + `mur-core/src/cli/actions.rs` (new subcommand — mirror `Stop`)
- Test: `mur-core/src/cmd/agent/restart.rs`

**Interfaces:**
- Consumes: `resolve_mur_home`, `LockFile` (pid), the stale predicate from Task 8 (or inline build-sha compare — see note).
- Produces: `pub fn cmd_restart(name: Option<&str>, all: bool, stale_only: bool, dry_run: bool) -> Result<()>`.

**Mechanism:** for each target agent, read `running.lock.pid`, send SIGTERM (the runtime drains via Task 5), wait for the pid to exit, then poll for a NEW `running.lock` (different pid — launchd `KeepAlive` respawns on the fresh binary), bounded ~30s. Report `old_sha → new_sha`. `--dry-run` lists targets without acting. Reuse the SIGTERM + pid-wait logic from `cmd_stop` (`lifecycle.rs:590-608`) but do NOT remove the lock (launchd respawns).

- [ ] **Step 1: Write the failing test** (dry-run target selection — no real kill)
```rust
    #[test]
    fn restart_dry_run_selects_stale_only() {
        // temp MUR_HOME with two running locks: one stale (build "old"), one
        // current (build == on-disk). --stale --dry-run names exactly the stale.
        // (Construct synthetic running.lock files; assert the printed/returned
        // target list. Factor target-selection into a pure helper
        // `select_targets(home, name, all, stale_only) -> Vec<String>` and test that.)
    }
```
(Write `select_targets` as a pure function and assert it returns only the stale agent for `stale_only=true`; the SIGTERM/respawn path is covered by the live operator test.)

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core restart_dry_run_selects_stale`
Expected: **compile error** — `select_targets`/`cmd_restart` missing.

- [ ] **Step 3: Implement `cmd_restart` + `select_targets`**

Create `mur-core/src/cmd/agent/restart.rs` with:
- `fn select_targets(home, name: Option<&str>, all: bool, stale_only: bool) -> Result<Vec<String>>` — enumerate running agents (those with a `running.lock`); filter to the named one, or all, or stale-only (stale = `lock.build_sha != on_disk_sha()`; see Task 8 for `on_disk_sha`).
- `pub fn cmd_restart(...)` — `select_targets`; if `dry_run`, print the list (+ stale reason) and return; else for each: SIGTERM the pid (mirror `lifecycle.rs:590-608`), wait for exit, poll for a fresh `running.lock` with a different pid (bounded 30s, 200ms poll), print `agent X restarted (old_sha → new_sha)`; continue past per-agent failures.

- [ ] **Step 4: Wire the CLI**

In `mur-core/src/cli/agent.rs` add a `Restart { name: Option<String>, #[arg(long)] all: bool, #[arg(long="stale")] stale: bool, #[arg(long="dry-run")] dry_run: bool }` variant (mirror the existing `Stop` variant), and in `cli/actions.rs` add the match arm dispatching to `cmd_restart`. Declare `mod restart;` + `pub use restart::cmd_restart;` in `cmd/agent/mod.rs`.

- [ ] **Step 5: Run test + compile**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core restart_dry_run_selects_stale && ORT_STRATEGY=download cargo check -p mur-core`
Expected: **PASS** + clean.

- [ ] **Step 6: Commit**
```bash
git add mur-core/src/cmd/agent/restart.rs mur-core/src/cmd/agent/mod.rs mur-core/src/cli/agent.rs mur-core/src/cli/actions.rs
git commit -m "feat(agent): mur agent restart <name>|--stale|--all [--dry-run] (graceful)"
```

---

## Phase D — ② Notify (Tasks 7–8)

### Task 7: `mur agent doctor` + on-disk sha helper

**Files:**
- Create: `mur-core/src/cmd/agent/doctor.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs` + `cli/{agent,actions}.rs`
- Test: `mur-core/src/cmd/agent/doctor.rs`

**Interfaces:**
- Produces: `pub fn on_disk_sha() -> String` (build-id of the runtime a restart would launch); `fn is_stale(lock: &LockFile, on_disk: &str) -> bool`; `pub fn cmd_doctor(json: bool) -> Result<()>`.

`on_disk_sha()`: resolve the runtime path via the existing `resolve_runtime_target()` (`cmd/agent/mod.rs:125`), run `<path> --build-id` (Task 3), trim stdout. Fallback `"unknown"` if the exec fails.

- [ ] **Step 1: Write the failing test (pure stale predicate)**
```rust
    #[test]
    fn is_stale_compares_build_sha() {
        let mut lock = sample_lock();        // helper: a LockFile with build_sha
        lock.build_sha = "abc123def456".into();
        assert!(is_stale(&lock, "999999999999"));      // differ → stale
        assert!(!is_stale(&lock, "abc123def456"));     // same → current
        // Two "unknown"s (git-less builds) compare equal → NOT stale (no spam).
        lock.build_sha = "unknown".into();
        assert!(!is_stale(&lock, "unknown"));
        // Old lock (empty build_sha) vs a known on-disk sha → stale.
        lock.build_sha = String::new();
        assert!(is_stale(&lock, "abc123def456"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core is_stale_compares_build_sha`
Expected: **compile error**.

- [ ] **Step 3: Implement**

`is_stale(lock, on_disk) -> bool { !lock.build_sha.is_empty() && lock.build_sha != on_disk || (lock.build_sha.is_empty() && on_disk != "unknown") }` — i.e. stale when the running build differs from on-disk, treating an empty (pre-feature) lock as stale against a known on-disk sha, and two `"unknown"`s as equal. (Write it to satisfy the four assertions exactly.) Plus `on_disk_sha()` (exec `--build-id`) and `cmd_doctor` (iterate running agents, print `name: running, current|STALE (lock <sha8> vs disk <sha8>)` + the `→ run 'mur agent restart <name>'` hint for stale ones; `--json` emits an array; exit non-zero if any stale).

- [ ] **Step 4: Run test + wire CLI**

Add a `Doctor { #[arg(long)] json: bool }` variant (mirror `Status`) in `cli/agent.rs` + `actions.rs`; `mod doctor;` + re-export in `mod.rs`.
Run: `ORT_STRATEGY=download cargo nextest run -p mur-core is_stale_compares_build_sha && ORT_STRATEGY=download cargo check -p mur-core`
Expected: **PASS** + clean.

- [ ] **Step 5: Commit**
```bash
git add mur-core/src/cmd/agent/doctor.rs mur-core/src/cmd/agent/mod.rs mur-core/src/cli/agent.rs mur-core/src/cli/actions.rs
git commit -m "feat(agent): mur agent doctor — flag stale runtimes (build-sha compare)"
```

---

### Task 8: `status` marker + post-build nudge

**Files:**
- Modify: `mur-core/src/cmd/agent/lifecycle.rs` (`cmd_status` ~`:462`)
- Modify: `build.sh` (after the `~/.local/bin/mur-agent-runtime` copy, ~`:87`)
- Test: covered by Task 7's `is_stale`; this task is wiring.

- [ ] **Step 1: Add the stale marker to `status`**

In `cmd_status` (`lifecycle.rs:462`), for each running agent, compute `is_stale(&lock, &on_disk_sha())` (reuse Task 7) and append ` — stale runtime (restart to apply)` to the agent's status line when stale.

- [ ] **Step 2: Post-build nudge in build.sh**

In `build.sh`, after the block that copies the runtime to `~/.local/bin/mur-agent-runtime` (~`:87`), add a print-only nudge:
```sh
  # Nudge: running agents keep their OLD process until restarted, so they're
  # still on the pre-upgrade runtime. Tell the operator (print-only; never auto-restart).
  if command -v mur >/dev/null 2>&1; then
    STALE=$(mur agent doctor --json 2>/dev/null | grep -c '"stale":true' || true)
    if [ "${STALE:-0}" -gt 0 ]; then
      echo "⚠ $STALE agent(s) are running a stale runtime — run 'mur agent restart --stale' (--dry-run to list)"
    fi
  fi
```
(Ensure `mur agent doctor --json` emits a `"stale":true|false` field per agent — adjust the grep to the actual JSON key from Task 7.)

- [ ] **Step 3: Verify**

Run: `ORT_STRATEGY=download cargo check -p mur-core`
Expected: clean. (The build.sh nudge is shell; verify by inspection — it's print-only and guarded by `command -v mur`.)

- [ ] **Step 4: Commit**
```bash
git add mur-core/src/cmd/agent/lifecycle.rs build.sh
git commit -m "feat(agent): stale-runtime marker in status + post-build nudge"
```

---

### Task 9: Workspace verification

- [ ] **Step 1: Lint + format**

Run: `cargo fmt --check && ORT_STRATEGY=download cargo clippy -p mur-common -p mur-core -p mur-agent-runtime -- -D warnings`
Expected: clean. (If fmt fails, `cargo fmt` + amend.)

- [ ] **Step 2: Targeted test sweep**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common -p mur-core a2a_dial && ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime task_runner`
Expected: **PASS**.

- [ ] **Step 3: Commit (if fmt changed anything)**
```bash
git add -A && git commit -m "chore: fmt"
```

---

## Live / operator verification (post-merge; needs running agents)

1. Build + install (`./install.sh`) so `mur` + `mur-agent-runtime` are on the same fresh sha.
2. `mur agent doctor` → agents started before this build show **STALE**.
3. `mur agent restart --stale` → each drains, launchd respawns; `doctor` flips them to **current**.
4. A `parallel_jobs`/`channel/delegate` dial that previously returned `-32601`/silent-fail now either succeeds (restarted) or returns the actionable **"run mur agent restart X"** error (not restarted).

---

## Self-Review

**Spec coverage:** §0 infra → Tasks 1-3; §① dial gate → Task 4; §③ drain → Task 5, restart → Task 6; §② doctor → Task 7, status+nudge → Task 8. One-time cutover (proto-0 gated) → Task 4 tests + the actionable error. Non-goals (auto-restart/re-exec/multi-version) → not implemented. ✓

**Placeholder scan:** The drain (Task 5) and restart/doctor CLI wiring (Tasks 6-7) reference reading the live `task_runner.rs` / `cli/*.rs` for exact `TaskOutcome`/enum shapes — these are integration points whose surrounding types must be read, not invented; the contract, integration line, and tests are exact. All bounded code (build.rs, constants, lock fields, dial gate, predicates) is complete.

**Type consistency:** `SHORT_SHA`/`A2A_PROTO_VERSION`/`method_min_proto` (Task 1-2) → used verbatim in Tasks 3-4. `LockFile.build_sha`/`proto_version` (Task 3) → read in Tasks 4,6,7. `is_stale`/`on_disk_sha` (Task 7) → reused in Tasks 6,8. `begin_drain`/`await_idle` (Task 5) → called in supervisor. ✓
