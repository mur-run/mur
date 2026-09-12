# Plan: heartbeats and handles — liveness is a frame, work duration is not a timeout

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §3.6 (D6, D7), §7; §9 step 5.
> Base: `main` at v2.79.0 or later (step 4 merged).

**Goal.** A router that thinks for ten minutes is alive the whole time and the dial knows it; a tool that launches long work returns a handle within a second and the caller polls `mur_job_status`.

**Architecture.** The runtime emits a `turn/heartbeat` JSON-RPC notification on the request's connection every 30 s while a turn runs (both `message/send` and `channel/delegate`), and advertises it by bumping `A2A_PROTO_VERSION` to 2 in `running.lock`. The dial in `mur-core` reads that proto: a peer at proto ≥ 2 gets a 90 s idle timeout (every heartbeat resets it), an older peer keeps 600 s — so a mixed-version fleet never starts failing at 90 s during inference. The fleet loop records itself as one `RunKind::Fleet` run (`run.json` + heartbeat + terminal state) under a caller-supplied `--run-id`, which is what makes a handle possible: the `fleet_run` tool generates the id, passes it, detaches the child, and returns `{run_id, status: "dispatched"}`; `parallel_jobs` in the MCP server does the same by detaching `execute_dag` onto the server's runtime. The MCP per-call default stays 120 s and says so.

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`; from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`.

## Behaviour change to state in the PR

- `fleet_run` (agent tool) and `parallel_jobs` (MCP tool) **no longer return the work's output**. They return a handle at once; progress and the result come from `mur_job_status <run_id>` / `mur fleet status <fleet>` / the fleet channel. A concierge that used to hand back the deep-research report inline now hands back the run id and must poll — the `deep-research` panel and the murmur fleet rail already show the run live, so the person watching loses nothing; an agent that consumed the text must be taught to poll (the tool description does that).
- A `parallel_jobs` run lives inside the MCP server process that dispatched it; if that process exits, the run dies with it. Stated in the tool description.

## Global Constraints (from the spec)

- §3.6: liveness = a heartbeat frame on the A2A stream **at least every 30 s while a turn is in progress, including during model inference**; the dial's idle timeout becomes 90 s. Work duration is not a timeout: `parallel_jobs` and `fleet_run` return `{run_id, status: "dispatched"}` within a second; the caller polls `mur_job_status`. The MCP per-call default stays 120 s, documented as "a tool that needs longer must return a handle".
- D7: never raise `DEFAULT_DIAL_IO_TIMEOUT` past 600 s — same failure, later.
- §7 / §4: heartbeat missing for 90 s → the dial fails with `agent X stopped responding` (not "went idle"), naming the last heartbeat time. `parallel_jobs` returns within 2 s with a `run_id` for a job that takes 60 s.
- D9: authorization gates (`fleet_run` allowlist, `parallel_jobs.targets`) are untouched.
- **Mixed versions are the normal state of this machine** (memory: agents lag the CLI for hours). The 90 s timeout applies only to a peer whose `running.lock` advertises proto ≥ 2. Never key it on the CLI's own version.
- A heartbeat is a JSON-RPC **notification** (no `id`). Both dial read loops already skip lines whose `id` does not match; verify, do not assume (Task 2 has the test).
- `pub fn` signature changes in `mur-core`/`mur-common` that the workspace-excluded Hub calls break the Hub: grep `mur-hub-gui/src-tauri/src` for every renamed/extended function; run the Hub `cargo check` last (symlink `mur-hub-gui/ui/dist` from the main checkout; remove it before committing).
- Before every commit: `cargo fmt`, `cargo clippy -p <crate> --all-targets -- -D warnings` (exit code, not grep), the named tests green.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-common/src/build.rs` | `A2A_PROTO_VERSION = 2`, `HEARTBEAT_MIN_PROTO = 2` | 1 |
| `mur-agent-runtime/src/protocol/heartbeat.rs` (new) | `HEARTBEAT_INTERVAL`, `HeartbeatGuard`, `spawn(notifier, task_id, every)`; tests | 1 |
| `mur-agent-runtime/src/protocol/mod.rs` | `pub mod heartbeat;` | 1 |
| `mur-agent-runtime/src/protocol/methods/message_send.rs`, `channel_delegate.rs` | hold a guard for the turn's lifetime | 1 |
| `mur-core/src/a2a_dial.rs` | `dial_io_timeout_for(&LockFile)`, 90 s vs 600 s, "stopped responding" error with last-frame time; tests | 2 |
| `mur-core/src/cmd/fleet/loop_run.rs` | `run_guarded(.., run_id: Option<String>)`, the loop's own `RunState` (save / heartbeat / terminal update); tests | 3 |
| `mur-core/src/cmd/fleet/run.rs`, `mur-core/src/cli/actions.rs`, `mur-core/src/dispatch.rs`, `mur-daemon/src/fleet_tick.rs`, `mur-core/src/cmd/deep_research/run.rs` | `--run-id` threaded; other callers pass `None` | 3 |
| `mur-agent-runtime/src/tools/fleet_run.rs` | generate `run_id`, `--run-id`, detach, log file, return the handle; tests | 4 |
| `mur-core/src/executor/jobs.rs` | `dispatch_parallel_jobs` (detach) beside `run_parallel_jobs`; test | 5 |
| `mur-mcp-server/src/tools.rs` | `parallel_jobs` returns the handle; description; test | 5 |
| `mur-agent-runtime/src/tools/mcp.rs`, `CLAUDE.md` | the 120 s rule in words | 6 |

---

## Task 1 — the runtime beats while it thinks

**Interfaces.**
- Consumes: `RequestContext.notifier: Option<mpsc::Sender<Value>>` (per connection, `unix_socket.rs`), `mur_common::build::A2A_PROTO_VERSION` (written into `running.lock` by the supervisor).
- Produces:

```rust
// mur_common::build
pub const A2A_PROTO_VERSION: u32 = 2;
/// The first proto whose runtimes emit `turn/heartbeat` (spec §3.6).
pub const HEARTBEAT_MIN_PROTO: u32 = 2;
// mur_agent_runtime::protocol::heartbeat
pub const HEARTBEAT_INTERVAL: std::time::Duration;           // 30 s
pub const HEARTBEAT_METHOD: &str = "turn/heartbeat";
pub struct HeartbeatGuard { .. }                                 // Drop stops the task
pub fn spawn(notifier: tokio::sync::mpsc::Sender<serde_json::Value>, task_id: String, every: std::time::Duration) -> HeartbeatGuard
```

  Frame: `{"jsonrpc":"2.0","method":"turn/heartbeat","params":{"task_id":"<id>","at":"<rfc3339>"}}`.

### Steps

- [ ] **1.1 Write the failing test** — new file `mur-agent-runtime/src/protocol/heartbeat.rs`, tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Frames arrive on the interval, carry the task id, and stop when the
    /// guard is dropped — the turn's end is the last beat.
    #[tokio::test]
    async fn beats_on_the_interval_and_stops_with_the_guard() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let guard = spawn(tx, "t-1".into(), std::time::Duration::from_millis(50));
        let mut seen = 0;
        while seen < 3 {
            let f = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("a beat within 2s")
                .expect("channel open");
            assert_eq!(f["method"], HEARTBEAT_METHOD);
            assert_eq!(f["params"]["task_id"], "t-1");
            assert!(f["params"]["at"].as_str().unwrap().contains('T'), "rfc3339: {f}");
            assert!(f.get("id").is_none(), "a heartbeat is a notification");
            seen += 1;
        }
        drop(guard);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(rx.try_recv().is_err(), "no beats after the guard is gone");
    }

    /// A closed connection ends the task instead of logging forever.
    #[tokio::test]
    async fn a_closed_sink_ends_the_task() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let guard = spawn(tx, "t-2".into(), std::time::Duration::from_millis(10));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(guard.handle.is_finished());
    }
}
```

- [ ] **1.2 Watch it fail** — add `pub mod heartbeat;` to `mur-agent-runtime/src/protocol/mod.rs`; `cargo nextest run -p mur-agent-runtime --lib -E 'test(/heartbeat::/)'`. Expected: `cannot find function spawn`.

- [ ] **1.3 Implement** — above the tests:

```rust
//! `turn/heartbeat` — proof of life while a turn runs (spec 2026-09-12
//! execution-limits §3.6, D7). The dial on the other end keeps a SHORT idle
//! timeout and never fires on a router that is merely thinking, because a
//! frame arrives every thirty seconds whether or not the model has produced
//! a token. Liveness is this frame; a bigger read timeout would be the same
//! failure, later.

use std::time::Duration;

use serde_json::{Value, json};

/// Half of the 60 s the spec allows between beats; three missed beats are
/// the dial's 90 s.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
pub const HEARTBEAT_METHOD: &str = "turn/heartbeat";

/// Stops the beat when dropped — hold it for exactly the turn's lifetime.
pub struct HeartbeatGuard {
    pub(crate) handle: tokio::task::JoinHandle<()>,
}

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Beat on `notifier` every `every` until dropped or the connection closes.
/// The first beat is one interval in — a turn shorter than that never beats,
/// which is fine: its reply arrives first.
pub fn spawn(
    notifier: tokio::sync::mpsc::Sender<Value>,
    task_id: String,
    every: Duration,
) -> HeartbeatGuard {
    let handle = tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await; // the immediate first tick
        loop {
            tick.tick().await;
            let frame = json!({
                "jsonrpc": "2.0",
                "method": HEARTBEAT_METHOD,
                "params": { "task_id": task_id, "at": chrono::Utc::now().to_rfc3339() },
            });
            if notifier.send(frame).await.is_err() {
                break; // connection gone — nobody to reassure
            }
        }
    });
    HeartbeatGuard { handle }
}
```

  `mur-common/src/build.rs`: `pub const A2A_PROTO_VERSION: u32 = 2;` and add `pub const HEARTBEAT_MIN_PROTO: u32 = 2;` with the doc line above; the existing `const { assert!(A2A_PROTO_VERSION >= 1) }` stays true. Do **not** add `turn/heartbeat` to `method_min_proto` — it is never dialed.

- [ ] **1.4 Hold the guard for the turn** —

  `message_send.rs`, inside the `Some(notifier) =>` arm, before `let outcome = self.runner.run_sync_streaming(...)`:

```rust
                // Proof of life for the dialing side while this turn runs —
                // including through model inference, when no delta flows.
                let _beat = turn_task_id.as_ref().map(|tid| {
                    crate::protocol::heartbeat::spawn(
                        notifier.clone(),
                        tid.clone(),
                        crate::protocol::heartbeat::HEARTBEAT_INTERVAL,
                    )
                });
```

  (`_beat` drops at the end of the arm, after `outcome` is computed — that is the turn's end.) The `None =>` arm has no connection to beat on; leave it.

  `channel_delegate.rs`: rename the handler parameter `_ctx` → `ctx` and, before `self.runner.mark_unattended(&task_id).await;`:

```rust
        // The fleet router dialing this delegate holds a 90 s idle timeout
        // (proto ≥ 2); beat so a long specialist turn is not mistaken for a
        // dead one.
        let _beat = ctx.notifier.as_ref().map(|n| {
            crate::protocol::heartbeat::spawn(
                n.clone(),
                task_id.clone(),
                crate::protocol::heartbeat::HEARTBEAT_INTERVAL,
            )
        });
```

  Then in `channel_delegate.rs` tests, add:

```rust
    /// The delegate beats on the connection that dialed it: a router waiting
    /// on a slow specialist sees a frame before its 90 s idle timeout.
    #[tokio::test]
    async fn a_delegated_turn_beats_on_the_callers_connection() {
        // Build the handler the way the existing tests in this module do
        // (copy their fixture verbatim), with a runner whose stub takes ~1s
        // (RunnerBackend::StubSlow is 60s — too long; use a SequenceLlm stub
        // with a tool that sleeps 300 ms, as task_runner's SlowVaryingTool does).
        // Then override the interval: expose `spawn` through a
        // `#[cfg(test)] pub(crate) static HEARTBEAT_INTERVAL_OVERRIDE: Mutex<Option<Duration>>`
        // read by both handlers, set it to 50 ms here, dial `channel/delegate`
        // with a RequestContext::with_notifier(tx), and assert at least one
        // frame with method "turn/heartbeat" and the task id arrives on rx
        // before the response.
    }
```

  Write that test out in full against the module's existing fixture (read the module's tests first — they already construct a `ChannelDelegateHandler` and a channel). If the override static is uglier than the fixture allows, an equivalent is to make the interval a field on both handlers with a `Default` of `HEARTBEAT_INTERVAL` and a `#[cfg(test)]` setter; choose one and use it in both handlers.

- [ ] **1.5 Watch it pass** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(/heartbeat|a_delegated_turn_beats/)'`. Then the whole crate.

- [ ] **1.6 fmt + clippy on `mur-agent-runtime`, `mur-common`**, then **commit**:

```
feat(runtime): turn/heartbeat — proof of life every 30 s while a turn runs

message/send and channel/delegate hold a HeartbeatGuard for the turn's
lifetime; a JSON-RPC notification lands on the dialing connection every
thirty seconds, through model inference included. A2A_PROTO_VERSION is 2
so running.lock advertises it; the dial reads that to pick its timeout.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — the dial trusts the beat, and only from a peer that beats

**Interfaces.**
- Consumes: `LockFile.proto_version` (already read in `dial_method`), `HEARTBEAT_MIN_PROTO` (Task 1), `HEARTBEAT_METHOD`.
- Produces (in `mur-core/src/a2a_dial.rs`):

```rust
const LEGACY_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(600);   // was DEFAULT_DIAL_IO_TIMEOUT
const HEARTBEAT_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(90);
fn dial_io_timeout_for(lock: &LockFile) -> Duration   // env override > proto ≥ 2 → 90 s > 600 s
```

  and a read-timeout error that says `agent 'X' stopped responding — no frame for 90s (last: <rfc3339> · <method>)` for a beating peer, the existing "did not respond within Ns" wording for a legacy one.

### Steps

- [ ] **2.1 Write the failing tests** — in the existing `timeout_tests` module of `a2a_dial.rs` (it owns `ENV_LOCK` and the fake-listener shape; copy the `stalled` fixture):

```rust
    /// A proto-2 peer that beats every 300 ms while it "thinks" for 2 s is
    /// alive: a 1 s idle timeout never fires, and the response arrives.
    #[test]
    fn heartbeats_reset_the_idle_timeout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("agent.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf);
                for _ in 0..7 {
                    std::thread::sleep(Duration::from_millis(300));
                    let _ = conn.write_all(
                        b"{\"jsonrpc\":\"2.0\",\"method\":\"turn/heartbeat\",\"params\":{\"task_id\":\"t\",\"at\":\"2026-09-12T00:00:00Z\"}}\n",
                    );
                }
                let _ = conn.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n");
            }
        });
        write_lock(tmp.path(), "beating", &sock_path, 2);
        unsafe { std::env::set_var("MUR_A2A_IO_TIMEOUT_SECS", "1") };
        let result = dial_method(tmp.path(), "beating", "message/send", serde_json::json!({}), DialMode::RequireRunning);
        unsafe { std::env::remove_var("MUR_A2A_IO_TIMEOUT_SECS") };
        let v = result.expect("heartbeats must keep the dial alive");
        assert_eq!(v["ok"], true);
        let _ = server.join();
    }

    /// A proto-2 peer that goes silent is reported as STOPPED RESPONDING,
    /// naming the last frame — not as "went idle", which is what a legacy
    /// peer with no heartbeats gets.
    #[test]
    fn a_silent_beating_peer_stopped_responding() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let sock_path = tmp.path().join("agent.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let _server = std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(
                    b"{\"jsonrpc\":\"2.0\",\"method\":\"turn/heartbeat\",\"params\":{\"task_id\":\"t\",\"at\":\"2026-09-12T00:00:00Z\"}}\n",
                );
                std::thread::sleep(Duration::from_secs(5));
            }
        });
        write_lock(tmp.path(), "silent", &sock_path, 2);
        unsafe { std::env::set_var("MUR_A2A_IO_TIMEOUT_SECS", "1") };
        let err = dial_method(tmp.path(), "silent", "message/send", serde_json::json!({}), DialMode::RequireRunning).unwrap_err();
        unsafe { std::env::remove_var("MUR_A2A_IO_TIMEOUT_SECS") };
        let msg = format!("{err:#}");
        assert!(msg.contains("stopped responding") && msg.contains("turn/heartbeat"), "{msg}");
    }

    /// The timeout is chosen by the PEER's proto: a legacy lock keeps 600 s,
    /// a beating one gets 90 s, and the env override beats both.
    #[test]
    fn idle_timeout_follows_the_peers_proto() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MUR_A2A_IO_TIMEOUT_SECS") };
        let mut lock = lock_fixture();
        lock.proto_version = 1;
        assert_eq!(dial_io_timeout_for(&lock), LEGACY_DIAL_IO_TIMEOUT);
        lock.proto_version = 2;
        assert_eq!(dial_io_timeout_for(&lock), HEARTBEAT_DIAL_IO_TIMEOUT);
        unsafe { std::env::set_var("MUR_A2A_IO_TIMEOUT_SECS", "7") };
        assert_eq!(dial_io_timeout_for(&lock), Duration::from_secs(7));
        unsafe { std::env::remove_var("MUR_A2A_IO_TIMEOUT_SECS") };
    }
```

  Add two helpers to the module: `fn write_lock(home: &Path, name: &str, sock: &Path, proto: u32)` (the JSON the `stalled` test writes, plus `"proto_version": proto`) and `fn lock_fixture() -> LockFile` (`serde_json::from_value` of the same JSON with a dummy socket). Rewrite the existing `stalled` test to use `write_lock(.., 1)` and keep its `did not respond` assertion — that is the legacy path.

- [ ] **2.2 Watch them fail** — `cargo nextest run -p mur-core --lib -E 'test(/timeout_tests::/)'`. Expected: compile errors for the new names.

- [ ] **2.3 Implement** — replace `DEFAULT_DIAL_IO_TIMEOUT` and `dial_io_timeout()`:

```rust
/// Idle read/write timeout for a peer that does NOT beat (proto < 2). Sits
/// above the runtime's default HITL wait (300 s) plus generation headroom;
/// never raise it — a beating peer is what makes a short timeout safe (D7).
const LEGACY_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(600);
/// Idle timeout for a peer that beats every 30 s (spec §3.6): three missed
/// beats. Every heartbeat, delta and step frame resets it.
const HEARTBEAT_DIAL_IO_TIMEOUT: Duration = Duration::from_secs(90);

/// The idle timeout for THIS peer, from its running.lock: the env override
/// (tests) wins; else a runtime that advertises heartbeats gets the short
/// one and anything older keeps the long one, so a fleet mid-upgrade never
/// starts failing at 90 s on a router that is merely thinking.
fn dial_io_timeout_for(lock: &LockFile) -> Duration {
    if let Some(d) = std::env::var("MUR_A2A_IO_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
    {
        return d;
    }
    if lock.proto_version >= mur_common::build::HEARTBEAT_MIN_PROTO {
        HEARTBEAT_DIAL_IO_TIMEOUT
    } else {
        LEGACY_DIAL_IO_TIMEOUT
    }
}
```

  `dial_socket` and `dial_message_streaming` both already parse `lock` — call `dial_io_timeout_for(&lock)` where they called `dial_io_timeout()`. In each read loop keep `let mut last_frame: Option<(String, String)> = None;` and, for every parsed line that is not the response, set it to `(method, params.at or now)`; on a read timeout build the error:

```rust
                if is_io_timeout(&e) {
                    if lock.proto_version >= mur_common::build::HEARTBEAT_MIN_PROTO {
                        anyhow!(
                            "agent '{agent_name}' stopped responding — no frame for {}s (last: {}); \
                             check `mur agent logs {agent_name}`",
                            timeout.as_secs(),
                            last_frame
                                .as_ref()
                                .map(|(m, at)| format!("{at} · {m}"))
                                .unwrap_or_else(|| "none since the request".into())
                        )
                    } else {
                        anyhow!("agent '{agent_name}' did not respond within {}s; check `mur agent logs {agent_name}`", timeout.as_secs())
                    }
                }
```

  (the streaming loop's wording is "went idle for Ns without a response" — keep that for legacy). Any other caller of `dial_io_timeout()` (grep) gets `dial_io_timeout_for(&lock)` if it has a lock, else `LEGACY_DIAL_IO_TIMEOUT` with a one-line comment.

- [ ] **2.4 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/a2a_dial::/)'`; then `command grep -rn "dial_io_timeout\b\|DEFAULT_DIAL_IO_TIMEOUT" mur-core mur-daemon mur-gui-core mur-hub-gui/src-tauri` must show only the new names.

- [ ] **2.5 fmt + clippy on `mur-core`**, then **commit**:

```
feat(dial): 90 s idle timeout for a peer that beats, 600 s for one that does not

The timeout is chosen by the PEER's running.lock proto, never by the CLI's
own version, so a fleet mid-upgrade keeps working. A silent proto-2 peer
is reported as "stopped responding" with the last frame's time and
method; a legacy peer keeps today's wording.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — the fleet loop is one run with a handle

**Interfaces.**
- Consumes: `run_status::{RunState, RunKind::Fleet, State, RUN_SCHEMA, store::{save, update}, heartbeat::Heartbeat}`, `RunsConfig.heartbeat_interval_secs`, `LoopStop`.
- Produces: `run_guarded(mur_home, name, max_iterations, deadline, budget_usd, run_id: Option<String>)`; `cmd_fleet_run_loop(.., run_id: Option<String>)`; `cmd_fleet_run(.., run_id: Option<String>)` (one-shot uses it as `DagExecOptions.run_id`); `mur fleet run --run-id <id>`; `pub fn loop_terminal_state(stop: LoopStop) -> run_status::State`; the loop's per-iteration DAG run ids become `{run_id}-{iteration}` so `mur job list` groups them.

### Steps

- [ ] **3.1 Write the failing tests** — `loop_run.rs` tests:

```rust
    /// The whole loop is one run: a record exists while it runs and ends in
    /// the state its stop implies, so `mur_job_status <run_id>` answers for a
    /// handle the caller minted before the loop started.
    #[tokio::test]
    async fn the_loop_records_itself_under_the_callers_run_id() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut f = bounds_fixture("dev");
        f.loop_cfg = Some(mur_common::fleet::FleetLoop { trigger: "manual".into(), max_iterations: 0, budget_usd: 0.0, deadline: String::new(), done_when: "queue-empty".into() });
        crate::cmd::fleet::store::save_fleet(home, &f).unwrap();
        mur_channel::ChannelService::open(home).unwrap().create_for_fleet("dev", "mur", &["pm".into()]).unwrap();
        let (stop, _, _) = run_guarded(home, "dev", None, None, None, Some("fleet-dev-abc".into())).await.unwrap();
        assert_eq!(stop, LoopStop::QueueDrained);
        let rec = crate::run_status::store::load(home, "fleet-dev-abc").unwrap().expect("recorded");
        assert_eq!(rec.kind, crate::run_status::RunKind::Fleet);
        assert_eq!(rec.state, crate::run_status::State::Done);
        assert!(rec.last_heartbeat_at.is_some());
        let status = crate::run_status::status_of(home, "fleet-dev-abc").unwrap().unwrap();
        assert_eq!(status.state, crate::run_status::State::Done);
    }

    #[test]
    fn stops_map_to_run_states() {
        use crate::run_status::State;
        assert_eq!(loop_terminal_state(LoopStop::Converged), State::Done);
        assert_eq!(loop_terminal_state(LoopStop::QueueDrained), State::Done);
        assert_eq!(loop_terminal_state(LoopStop::AwaitingApproval), State::Blocked);
        assert_eq!(loop_terminal_state(LoopStop::Stopped), State::Stopped);
        assert_eq!(loop_terminal_state(LoopStop::CommanderKilled), State::Stopped);
        for s in [LoopStop::Deadline, LoopStop::Stuck, LoopStop::Budget, LoopStop::MaxIterations] {
            assert_eq!(loop_terminal_state(s), State::Failed, "{s:?}");
        }
    }
```

  and update `run_loop_for_test` to pass `None` as the new last argument.

- [ ] **3.2 Watch them fail** — `cargo nextest run -p mur-core --lib -E 'test(/the_loop_records_itself|stops_map_to_run_states/)'`.

- [ ] **3.3 Implement** — in `loop_run.rs`:

```rust
/// The run state a stop implies — the same map `terminal_state_for` gives
/// the channel, in the run ledger's vocabulary.
pub fn loop_terminal_state(stop: LoopStop) -> crate::run_status::State {
    use crate::run_status::State;
    match stop {
        LoopStop::Converged | LoopStop::QueueDrained => State::Done,
        LoopStop::AwaitingApproval => State::Blocked,
        LoopStop::Stopped | LoopStop::CommanderKilled => State::Stopped,
        LoopStop::MaxIterations | LoopStop::Deadline | LoopStop::Stuck | LoopStop::Budget => State::Failed,
    }
}
```

  `run_guarded` gains `run_id: Option<String>`. Where `RunProgress.run_id` is set (`run_id: uuid::Uuid::now_v7().to_string()`), compute once above it: `let run_id = run_id.unwrap_or_else(|| format!("fleet-{name}-{}", uuid::Uuid::now_v7()));` and use it for progress too. Right after `lock_progress(&progress).save(mur_home, name);`:

```rust
    // The loop is one run with a handle (spec §3.6): recorded now so
    // `mur_job_status <run_id>` answers while it runs, beaten every interval,
    // and closed with the state the stop implies. Best-effort like
    // progress.json — a ledger failure never stops the loop.
    let runs_cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).runs;
    let now = chrono::Utc::now();
    let record = crate::run_status::RunState {
        schema: crate::run_status::RUN_SCHEMA,
        run_id: run_id.clone(),
        channel_id: Some(fleet.channel_id.clone()),
        kind: crate::run_status::RunKind::Fleet,
        label: format!("fleet {name} loop"),
        pid: std::process::id(),
        started_at: now,
        last_heartbeat_at: Some(now),
        state: crate::run_status::State::Running,
        steps: vec![],
        blocked_on: None,
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        build_sha: mur_common::build::SHORT_SHA.to_string(),
    };
    let _loop_beat = match crate::run_status::store::save(mur_home, &record) {
        Ok(()) => Some(crate::run_status::heartbeat::Heartbeat::spawn(
            mur_home.to_path_buf(),
            run_id.clone(),
            std::time::Duration::from_secs(runs_cfg.heartbeat_interval_secs),
        )),
        Err(error) => {
            tracing::warn!(run_id = %run_id, %error, "fleet loop: run record not written; mur_job_status will not see this loop");
            None
        }
    };
```

  (no sidecar: the parent has no single first channel seq; `mur fleet status` keeps finding the per-iteration runs through theirs). The per-iteration `DagExecOptions.run_id` becomes `format!("{run_id}-{iteration}")`. Where the loop ends (next to `emit_stop_event(...)`):

```rust
    let terminal = loop_terminal_state(stop);
    if let Err(error) = crate::run_status::store::update(mur_home, &run_id, |r| r.state = terminal) {
        tracing::warn!(run_id = %run_id, %error, "fleet loop: terminal state not recorded");
    }
```

  (check `Heartbeat`'s drop semantics in `run_status/heartbeat.rs` — if it needs an explicit `stop()`, call it before the update so a late beat cannot resurrect `Running`.)

  `cmd_fleet_run_loop` gains `run_id: Option<String>` and forwards it. `cmd_fleet_run` (one-shot, `run.rs`) gains `run_id: Option<String>` and uses `run_id.unwrap_or_else(|| format!("run-{}", uuid::Uuid::now_v7()))` for `DagExecOptions.run_id`. CLI (`actions.rs` `FleetAction::Run`): add

```rust
        /// Record this run under a caller-chosen id (a tool that dispatched it
        /// and will poll `mur_job_status`). Default: a fresh id.
        #[arg(long, value_name = "RUN_ID")]
        run_id: Option<String>,
```

  and thread it in `dispatch.rs` to both calls. Every other caller (`fleet_tick.rs`, `deep_research/run.rs`, any test) passes `None` — let the compiler list them. Validate the id: same charset as a fleet name plus `-` and digits (`mur_common::fleet::valid_fleet_name` is too strict for a uuid; write `fn valid_run_id(s: &str) -> bool { !s.is_empty() && s.len() <= 96 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') }` in `run_status/mod.rs` with a two-line test, and `bail!` in `cmd_fleet_run*` on a bad one).

- [ ] **3.4 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/loop_run::|run_status::/)'`, `cargo nextest run -p mur-daemon`.

- [ ] **3.5 fmt + clippy on `mur-core`, `mur-daemon`**, then **commit**:

```
feat(fleet): the loop is one run with a handle — --run-id, run.json, heartbeat, terminal state

A caller can mint the id before the loop starts and poll mur_job_status
for it; the loop records itself as RunKind::Fleet, beats on the runs
heartbeat interval, and closes with the state its stop implies. The
per-iteration DAG runs are named under it.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — `fleet_run` returns a handle

**Interfaces.**
- Consumes: `--run-id` (Task 3), `run_status::store::run_path` layout (`~/.mur/runs/<run_id>/`), the existing spawn + signing-handoff code in `tools/fleet_run.rs`.
- Produces: tool output `{"run_id","fleet","status":"dispatched","log":"<path>","follow":"mur_job_status <run_id> · mur fleet status <fleet>"}` within a second; the child is detached (`kill_on_drop(false)`, stdio to the log file); `timeout_secs` input removed from the schema (accepted and ignored with a note in the output for one release).

### Steps

- [ ] **4.1 Write the failing test** — in `fleet_run.rs` tests. Read the module's existing tests first: they build a `FleetRunTool` and point `exec_dirs::mur_cli()` at a fake binary — reuse exactly that mechanism (grep `fn mur_cli` in `exec_dirs.rs` for the test override; if there is none, the existing tests do not exercise `execute`, and this test must set one up the same way `bash.rs` tests fake a binary: a shell script written to a temp dir, made executable, and the override env/var the module offers). The fake `mur` records its argv to a file and sleeps 5 s:

```rust
    /// §3.6: the tool returns a handle within a second while the fleet keeps
    /// running, and the child got the id it will be polled under.
    #[tokio::test]
    async fn fleet_run_returns_a_handle_and_leaves_the_child_running() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // config.yaml allowlist exactly as the existing tests write it
        write_allowlist(home, "mur", "deep-research");
        let argv_log = home.join("argv.txt");
        let fake = fake_mur_that_sleeps(home, &argv_log, 5);   // helper: script `echo "$@" > argv.txt; sleep 5`
        let tool = fleet_run_tool_for_test(home, "mur", &fake); // helper: the module's own constructor + the cli override
        let t0 = std::time::Instant::now();
        let out = tool.execute(serde_json::json!({"fleet": "deep-research", "goal": "why is the sky blue"})).await.unwrap();
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "returned in {:?}", t0.elapsed());
        let v: serde_json::Value = serde_json::from_str(&out.content).expect("json handle");
        assert_eq!(v["status"], "dispatched");
        let run_id = v["run_id"].as_str().unwrap().to_string();
        assert!(run_id.starts_with("fleet-deep-research-"), "{run_id}");
        assert!(v["follow"].as_str().unwrap().contains("mur_job_status"));
        // the child is still alive and was told the id
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let argv = std::fs::read_to_string(&argv_log).unwrap();
        assert!(argv.contains(&format!("--run-id {run_id}")), "{argv}");
        assert!(std::path::Path::new(v["log"].as_str().unwrap()).exists(), "log file created");
    }
```

- [ ] **4.2 Watch it fail** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(fleet_run_returns_a_handle)'`.

- [ ] **4.3 Implement** — in `execute`:
  - `let run_id = format!("fleet-{fleet}-{}", uuid::Uuid::now_v7());`
  - args: append `"--run-id".into(), run_id.clone()` to every arm (the `deep-research <q>` arm too — confirm `mur deep-research` accepts `--run-id` after Task 3's `cmd_fleet_run_loop` change; if `deep_research/run.rs` has its own clap surface, add the same flag there and forward it).
  - log: `let log_dir = self.mur_home.join("runs").join(&run_id); std::fs::create_dir_all(&log_dir)?; let log = std::fs::File::create(log_dir.join("fleet_run.log"))?;` — `runs/` is inside the fleet_run carve-in already (the child writes `run.json` there today); `.stdout(log.try_clone()?)`, `.stderr(log)`, `.kill_on_drop(false)`.
  - keep the signing handoff exactly as is (stdin pipe, write, shutdown).
  - do **not** await the child: `drop(child)` after the handoff, return

```rust
        Ok(serde_json::json!({
            "run_id": run_id,
            "fleet": fleet,
            "status": "dispatched",
            "log": log_dir.join("fleet_run.log"),
            "follow": format!("mur_job_status {run_id} · mur fleet status {fleet} — the result lands in the fleet channel, not in this reply"),
        }).to_string().into())
```

  - `timeout_secs`: remove from `input_schema`; if a caller still sends it, add `"note": "timeout_secs is ignored since 2.80 — the run is bounded by the fleet's limits (mur limits <fleet>)"` to the output. Delete `DEFAULT_TIMEOUT_SECS`, `MAX_TIMEOUT_SECS`, `resolve_timeout_secs` and their test.
  - description: `"Dispatch a MUR fleet (agent squad) and return a handle at once: {run_id, status: dispatched}. Poll mur_job_status <run_id> (or mur fleet status <fleet>) for progress; the result lands in the fleet's channel. For the deep-research fleet pass the research question as goal. Only fleets allowlisted in the user's config can be run; the run is bounded by the fleet's limits (deadline / stuck / cost_usd) and mur fleet stop."`

- [ ] **4.4 Watch it pass**, then the whole crate. Real-machine check (documented, not automated): from murmur ask the concierge to run deep-research; it must answer with the run id within seconds and `mur fleet status deep-research` must show the run.

- [ ] **4.5 fmt + clippy**, then **commit**:

```
feat(tools): fleet_run returns a handle — {run_id, status: dispatched} within a second

The child is detached with its stdio in ~/.mur/runs/<run_id>/fleet_run.log
and named under --run-id, so mur_job_status answers for it. timeout_secs is
gone: the run is bounded by the fleet's limits, not by how long the tool
call may block. Behaviour change — the report no longer comes back inline.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 5 — `parallel_jobs` returns a handle

**Interfaces.**
- Consumes: `executor::jobs::{run_parallel_jobs, resolve_jobs, authorize_targets, build_jobs_procedure}`, `DagExecOptions`, `run_status`.
- Produces (in `mur-core/src/executor/jobs.rs`):

```rust
pub struct Dispatched { pub run_id: String, pub channel_id: String, pub handle: tokio::task::JoinHandle<anyhow::Result<PipelineOutput>> }
pub fn dispatch_parallel_jobs(mur_home: &Path, jobs: &[Job], max_concurrency: Option<usize>, yes: bool) -> anyhow::Result<Dispatched>
```

  `run_parallel_jobs` stays (it is `dispatch` + await the handle) for any in-process caller that wants to wait. The MCP tool returns `{"run_id","channel_id","status":"dispatched","jobs":n,"follow":"mur_job_status <run_id>"}`.

### Steps

- [ ] **5.1 Write the failing tests** — `jobs.rs`:

```rust
    /// §7: dispatch returns at once with an id; the run is recorded under it
    /// and finishes on its own. The target is not running, so the run fails
    /// fast — what is under test is the shape, not the delegation.
    #[tokio::test]
    async fn dispatch_returns_before_the_run_finishes_and_records_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // authorize_targets reads config.yaml parallel_jobs.targets — write it the way the existing tests here do
        write_targets(home, &["ghost"]);
        let jobs = vec![Job { description: "do x".into(), assignee: "ghost".into() }];
        let t0 = std::time::Instant::now();
        let d = dispatch_parallel_jobs(home, &jobs, Some(1), false).unwrap();
        assert!(t0.elapsed() < std::time::Duration::from_secs(2));
        assert!(d.run_id.starts_with("run-"));
        let _ = d.handle.await;
        let rec = crate::run_status::store::load(home, &d.run_id).unwrap().expect("recorded by execute_dag");
        assert!(rec.state.is_terminal(), "{:?}", rec.state);
    }
```

  and in `mur-mcp-server/src/tools.rs` tests, next to `mur_job_status_reports_a_recorded_run`:

```rust
    /// The tool answers with a handle, not with output.
    #[tokio::test]
    async fn parallel_jobs_returns_a_dispatch_handle() {
        // same home/env setup as the neighbouring test; one job to an
        // authorized-but-not-running agent
        let out = call_tool_for_test("parallel_jobs", json!({"jobs": [{"description": "x", "agent": "ghost"}]})).await.unwrap();
        assert_eq!(out["status"], "dispatched");
        assert!(out["run_id"].as_str().unwrap().starts_with("run-"));
        assert!(out["follow"].as_str().unwrap().contains("mur_job_status"));
    }
```

- [ ] **5.2 Watch them fail.**

- [ ] **5.3 Implement** — `jobs.rs`:

```rust
/// Start the fan-out and return its handle at once (spec §3.6): the caller
/// polls `mur_job_status`. The run executes on the CURRENT tokio runtime —
/// inside `mur-mcp-server` that is the server's own lifetime, which the tool
/// description says out loud.
pub fn dispatch_parallel_jobs(
    mur_home: &Path,
    jobs: &[Job],
    max_concurrency: Option<usize>,
    yes: bool,
) -> Result<Dispatched> {
    authorize_targets(mur_home, jobs)?;
    let proc = build_jobs_procedure(jobs);
    let svc = ChannelService::open(mur_home)?;
    let channel_id = svc.create_for_workflow("parallel-jobs")?.id;
    let run_id = format!("run-{}", uuid::Uuid::now_v7());
    let home = mur_home.to_path_buf();
    let (rid, cid, label) = (run_id.clone(), channel_id.clone(), format!("{} parallel job(s)", jobs.len()));
    let handle = tokio::spawn(async move {
        let opts = DagExecOptions {
            yes,
            trigger: "agent",
            channel_id: Some(cid.clone()),
            run_id: rid,
            run_kind: Some(crate::run_status::RunKind::Job),
            run_label: label,
            max_concurrency,
            ..Default::default()
        };
        execute_dag(&home, "job:parallel-jobs", &proc, &opts)
            .await
            .map_err(|e| anyhow::anyhow!("parallel_jobs run on channel {cid} failed: {e}"))
    });
    Ok(Dispatched { run_id, channel_id, handle })
}

pub async fn run_parallel_jobs(mur_home: &Path, jobs: &[Job], max_concurrency: Option<usize>, yes: bool) -> Result<(String, PipelineOutput)> {
    let d = dispatch_parallel_jobs(mur_home, jobs, max_concurrency, yes)?;
    let out = d.handle.await.map_err(|e| anyhow::anyhow!("parallel_jobs task panicked: {e}"))??;
    Ok((d.channel_id, out))
}
```

  (`DagExecOptions<'a>` borrows `trigger: &'a str` — `"agent"` is `'static`, and `env_class_override` is `None`, so the options can live inside the spawned task; if the borrow checker objects to `proc` or `jobs`, clone `proc` before the spawn — `build_jobs_procedure` returns an owned value.)

  `tools.rs` `"parallel_jobs"` arm: call `dispatch_parallel_jobs`, `drop(d.handle)` is **not** allowed to cancel the task (a `JoinHandle` drop detaches — confirm in the tokio docs comment you leave), return

```rust
            Ok(json!({
                "run_id": d.run_id,
                "channel_id": d.channel_id,
                "status": "dispatched",
                "jobs": jobs.len(),
                "follow": format!("mur_job_status {} — output lands in channel {}", d.run_id, d.channel_id),
            }))
```

  Description: `"Fan out N distinct jobs to running MUR agents in parallel and return a handle at once: {run_id, channel_id, status: dispatched}. Poll mur_job_status <run_id> for progress; each job's reply lands in the channel. The run lives in this MCP server process — if the server exits, the run ends. Before coding fan-out, apply the parallel-code gate: disjoint files (no shared registry/lockfile), contracts frozen first, one writer per file. Targets the agents you name; runtimes must already be running."` Update the `mur_job_status` description's first sentence to drop "Use this after a tool call times out" in favour of "Use this after parallel_jobs / fleet_run hand you a run_id".

- [ ] **5.4 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/executor::jobs::/)'`, `cargo nextest run -p mur-mcp-server`.

- [ ] **5.5 fmt + clippy on `mur-core`, `mur-mcp-server`**, then **commit**:

```
feat(mcp): parallel_jobs returns a handle — the run is dispatched, not awaited

dispatch_parallel_jobs starts execute_dag on the server's runtime and
returns {run_id, channel_id, status: dispatched}; run_parallel_jobs is
dispatch + await for in-process callers. Behaviour change — output no
longer comes back in the tool result; mur_job_status and the channel do.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 6 — the 120 s rule, in words

- [ ] **6.1** `mur-agent-runtime/src/tools/mcp.rs`, the doc on `DEFAULT_MCP_TOOL_TIMEOUT_SECS`:

```rust
/// Default per-tool-call timeout when an MCP server entry sets no
/// `timeout_secs`. Deliberately short (spec 2026-09-12 execution-limits
/// §3.6): a tool that needs longer must return a handle and let the caller
/// poll — `fleet_run` and `parallel_jobs` do. Raising this is the wrong fix
/// for the next twenty-minute tool. Override per server via
/// `McpServerEntry.timeout_secs` when a server genuinely answers slowly.
```

- [ ] **6.2** `CLAUDE.md`, the `mur fleet` bullet: after the `mur limits` line add `- Long work returns a handle: \`fleet_run\` / \`parallel_jobs\` answer \`{run_id, status: dispatched}\` within a second and the caller polls \`mur_job_status\`; the MCP per-call timeout stays 120 s on purpose. The runtime beats \`turn/heartbeat\` every 30 s during a turn and the dial gives a beating peer (proto ≥ 2) 90 s idle, a legacy one 600 s.`
- [ ] **6.3** `mur verify --file CLAUDE.md` shows no new ❌ (the three pre-existing paths remain). Commit:

```
docs: the 120 s MCP rule and heartbeats, in words

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- One PR: `feat: heartbeats and handles — 90 s liveness, dispatch-and-poll for fleet_run / parallel_jobs (spec step 5)`. The body states both behaviour changes from the header of this plan and that the 90 s timeout applies only to peers at proto ≥ 2.
- Real-machine checks after install + `mur update --restart-agents` (agents must be on proto 2 before the 90 s path is even taken): (1) `mur agent send rustsmith "think for a while: ..."` against a slow model — no `did not respond` at 90 s; `mur agent logs` shows no error; (2) from murmur, `fleet_run deep-research` via the concierge answers with a run id in seconds and `mur fleet status deep-research` / `mur job status <id>` show it running then done; (3) `parallel_jobs` from Claude Code returns a handle; `mur_job_status` reports it; (4) kill one agent mid-turn — the dial reports `stopped responding — no frame for 90s (last: …)`.
- `update-docs`: agent-cli / fleet-loops pages mention the handle; the `mur` MCP skill (`mur-fleet-manage`, `parallel_jobs` docs) say "poll `mur_job_status`".
- Next: spec step 6 (dispatch preflight + `ToolError::NotAuthorized`), Hub 3b.
