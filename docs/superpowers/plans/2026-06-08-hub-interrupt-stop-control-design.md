# Hub Interrupt / Stop Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **⚠️ STATUS — SEQUENCED AFTER PHASE 3/4.** The source spec
> (`docs/superpowers/specs/2026-06-08-hub-interrupt-stop-control-design.md`) is marked **DEFERRED**:
> the BLOCKER task (Task 1, cancellation in `run_sync_inner`) lands in the exact
> `task_runner.rs::run_sync_inner` / `run_agentic_loop` region that the in-flight **Phase 3** (real
> HITL trigger) and **Phase 4** (tool ecosystem) work is rewriting. **Do not start Task 1 until those
> merge.** Run **Task 0 (pre-flight re-anchor)** first — it re-verifies every line number below against
> the then-current tree, because Phase 3/4 will shift them. Tasks 2–6 (the `TaskSpec` field, the Hub
> registry, the Stop button) are additive and lower-risk, but their tests depend on Task 1 to prove
> end-to-end cancel, so the whole plan executes as one sequence once unblocked.

**Goal:** Give the MUR Hub chat a **Stop** control (button + Esc) that cancels an agent's in-flight `message/send` generation via a client-supplied task id and the existing `tasks/cancel` RPC, preserving the partial streamed reply.

**Architecture:** The Hub generates a UUID task id per send and passes it in `message/send` params. The runner honors the caller-supplied id (instead of generating its own) and — the core new capability — registers a cancel oneshot and `select!`s generation against it inside `run_sync_inner`, so `tasks/cancel{id}` makes the in-flight LLM future drop and the task return `Cancelled`. The Hub keys an in-flight registry by agent name, dials `tasks/cancel` on a separate connection, and commits its own streamed buffer (tagged "stopped") as the reply.

**Tech Stack:** Rust (`mur-agent-runtime`, `mur-core`, `tokio` oneshot + `select!`, `uuid`), Tauri 2 (`mur-hub-gui/src-tauri`, managed state), React + TypeScript + Vitest (`mur-hub-gui/ui`).

**Test commands (this repo):** Rust tests run under **nextest** (plain `cargo test --workspace` flakes 7 mur-core tests — targeted crate runs are fine). Use `cargo nextest run -p <crate> <test>`; `cargo test -p <crate> <test>` is an acceptable fallback for a single test. UI tests: `cd mur-hub-gui/ui && npx vitest run <file>`.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `mur-agent-runtime/src/task_runner.rs` | Modify | Add `TaskSpec.task_id`; honor it in `run_sync_inner` id line; **register cancel signal + `select!` generation vs cancel + cleanup** (the BLOCKER). |
| `mur-agent-runtime/src/protocol/methods/message_send.rs` | Modify | Read `params["task_id"]` into `TaskSpec.task_id` (additive, back-compatible). |
| `mur-hub-gui/src-tauri/src/chat.rs` | Modify + add command | In-flight registry; `agent_chat_send` accepts `taskId`, stores it, passes `task_id` in params, clears on completion; new `agent_chat_cancel` command. |
| `mur-hub-gui/src-tauri/src/lib.rs` | Modify | `.manage(ChatRegistryState::default())`; register `chat::agent_chat_cancel` in `invoke_handler`. |
| `mur-hub-gui/ui/src/components/ChatTab.tsx` | Modify | Generate `taskId` per send; pass to `agent_chat_send`; **Stop** button while `busy` + Esc; on Stop call `agent_chat_cancel` and commit `streamingRef` as an `agent` message tagged "stopped". |

Each task below is self-contained and committed independently.

---

## Task 0: Pre-flight re-anchor (do FIRST, after Phase 3/4 merges)

**Files:** read-only verification — no edits.

This plan's line numbers were verified against `origin/main` (`task_runner.rs` = 1010 lines) on 2026-06-08. Phase 3/4 rewrites `run_sync_inner`/`run_agentic_loop`; re-verify before editing.

- [ ] **Step 1: Confirm the cancellation gap still exists**

Run:
```bash
grep -n "fn run_sync_inner\|fn start_async\|cancel_signals\|let id = format!(\"task-\|not cancellable\|struct TaskSpec\|context_task_id" mur-agent-runtime/src/task_runner.rs
```
Expected: `run_sync_inner` still has a bare `let id = format!("task-{}", Uuid::now_v7());` followed by a `match &self.backend` that is **not** wrapped in `tokio::select!` against a cancel signal, and `cancel_signals.insert(...)` appears **only** inside `start_async`. If Phase 3/4 already added cancellation to `run_sync_inner`, **stop and revise this plan** — Task 1 may be partially done.

- [ ] **Step 2: Record the current anchor lines**

Note the current line numbers for: `struct TaskSpec`, the `run_sync_inner` `let id =` line, the `match &self.backend` block start/end, the `message_send.rs` `TaskSpec { input, context_task_id }` construction, and the `chat.rs` `agent_chat_send` signature. Use these — not the historical numbers in this plan — when editing.

- [ ] **Step 3: Confirm the Hub anchors**

Run:
```bash
grep -n "agent_chat_send\|context_task_id\|\.manage(\|invoke_handler\|generate_handler" mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/src/chat.rs
grep -n "agent_chat_send\|streamingRef\|setBusy\|onKeyDown\|taskIdRef" mur-hub-gui/ui/src/components/ChatTab.tsx
```
Expected: `agent_chat_send` registered at the `invoke_handler` list; `chat.rs` has no managed registry yet; `ChatTab.tsx` has `streamingRef`, `busy`/`setBusy`, `onKeyDown`. No commit for this task (verification only).

---

## Task 1: Runtime — cancellable `run_sync_inner` (BLOCKER)

Add real cancellation to the production streaming path. Mirror the existing `start_async` pattern (register `cancel_signals[id]`, `select!` work vs `rx_cancel`), but for the inline `run_sync_inner` return-value style.

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (`struct TaskSpec` ~line 22; `run_sync_inner` ~lines 284–347)
- Test: `mur-agent-runtime/src/task_runner.rs` (`#[cfg(test)]` module in the same file — follow the existing test layout there)

### 1a — `TaskSpec.task_id` field

- [ ] **Step 1: Write the failing test**

Add to the test module in `task_runner.rs`:
```rust
#[test]
fn task_spec_accepts_optional_task_id() {
    let spec = TaskSpec {
        input: Message::user_text("hi"),
        context_task_id: None,
        task_id: Some("task-fixed-1".to_string()),
    };
    assert_eq!(spec.task_id.as_deref(), Some("task-fixed-1"));
}
```
(If `Message::user_text` is not the local constructor, copy the `Message { .. }` literal used by the nearest existing test in this file.)

- [ ] **Step 2: Run test — verify it fails to compile**

Run: `cargo nextest run -p mur-agent-runtime task_spec_accepts_optional_task_id`
Expected: FAIL — `struct TaskSpec has no field named task_id`.

- [ ] **Step 3: Add the field**

In `task_runner.rs`, extend `TaskSpec`:
```rust
#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub input: Message,
    pub context_task_id: Option<String>,
    /// Caller-supplied task id. When `Some`, the runner uses it verbatim so the
    /// client can cancel by an id it already holds; when `None` the runner
    /// generates one (back-compatible).
    pub task_id: Option<String>,
}
```

- [ ] **Step 4: Fix all existing `TaskSpec { .. }` constructors**

Run: `grep -rn "TaskSpec {" mur-agent-runtime/src`
For every constructor found (notably `message_send.rs` and any test helpers), add `task_id: None,`. Task 4 changes the `message_send.rs` one to read params — for now `None` keeps it compiling.

- [ ] **Step 5: Run test — verify it passes**

Run: `cargo nextest run -p mur-agent-runtime task_spec_accepts_optional_task_id`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs mur-agent-runtime/src/protocol/methods/message_send.rs
git commit -m "feat(runtime): add optional caller-supplied TaskSpec.task_id"
```

### 1b — Honor the supplied id in `run_sync_inner`

- [ ] **Step 1: Write the failing test**

The `StubEcho` backend returns synchronously, so this test asserts the returned task id equals the supplied id:
```rust
#[tokio::test]
async fn run_sync_uses_supplied_task_id() {
    let runner = TaskRunner::new(RunnerBackend::StubEcho); // match the local constructor
    let spec = TaskSpec {
        input: Message::user_text("hi"),
        context_task_id: None,
        task_id: Some("task-supplied-9".to_string()),
    };
    let outcome = runner.run_sync(spec).await;
    let TaskOutcome::Completed(task) = outcome else { panic!("expected Completed") };
    assert_eq!(task.id, "task-supplied-9");
}
```
(Use whatever `TaskRunner` constructor the existing tests use; `run_sync` is the non-streaming wrapper around `run_sync_inner`.)

- [ ] **Step 2: Run test — verify it fails**

Run: `cargo nextest run -p mur-agent-runtime run_sync_uses_supplied_task_id`
Expected: FAIL — `task.id` is a generated `task-<uuid>`, not `task-supplied-9`.

- [ ] **Step 3: Honor the id**

In `run_sync_inner`, replace the id line (~`:295`):
```rust
let id = format!("task-{}", Uuid::now_v7());
```
with:
```rust
let id = spec
    .task_id
    .clone()
    .unwrap_or_else(|| format!("task-{}", Uuid::now_v7()));
```

- [ ] **Step 4: Run test — verify it passes**

Run: `cargo nextest run -p mur-agent-runtime run_sync_uses_supplied_task_id`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): honor caller-supplied id in run_sync_inner"
```

### 1c — Register cancel signal + `select!` generation vs cancel

This is the core blocker. The `StubSlow` backend sleeps 60s, giving a deterministic, LLM-free cancel test.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn run_sync_streaming_is_cancellable_by_id() {
    use std::sync::Arc;
    let runner = Arc::new(TaskRunner::new(RunnerBackend::StubSlow));
    let (tx, _rx) = tokio::sync::mpsc::channel(8); // streaming sink, unused here
    let spec = TaskSpec {
        input: Message::user_text("slow"),
        context_task_id: None,
        task_id: Some("task-cancelme".to_string()),
    };
    let r2 = runner.clone();
    let handle = tokio::spawn(async move { r2.run_sync_streaming(spec, tx).await });

    // Let the task register its cancel signal, then cancel by the known id.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    runner.cancel("task-cancelme").await.expect("cancel should succeed");

    let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("must finish promptly, not wait 60s")
        .expect("join");
    let TaskOutcome::Cancelled(task) = outcome else { panic!("expected Cancelled, got {outcome:?}") };
    assert_eq!(task.id, "task-cancelme");
    assert_eq!(task.state, TaskState::Cancelled);
}
```

- [ ] **Step 2: Run test — verify it fails**

Run: `cargo nextest run -p mur-agent-runtime run_sync_streaming_is_cancellable_by_id`
Expected: FAIL — `cancel(...)` returns `Err("task task-cancelme not cancellable")` (id never registered), so the unwrap panics; even if that were tolerated, the task would `Completed` after 60s and the 2s timeout would trip.

- [ ] **Step 3: Implement cancellation in `run_sync_inner`**

Replace the body from the id line through the backend `match` (the block that currently assigns `let result: Result<Message, TaskError> = match &self.backend { … };`) with:

```rust
let id = spec
    .task_id
    .clone()
    .unwrap_or_else(|| format!("task-{}", Uuid::now_v7()));
self.set_state(&id, TaskState::Working);

// Register a cancel signal so `tasks/cancel{id}` can abort this in-flight
// generation. Mirrors `start_async`, but for the inline return-value path.
let (tx_cancel, mut rx_cancel) = oneshot::channel::<()>();
self.cancel_signals
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .insert(id.clone(), tx_cancel);

let generation = async {
    match &self.backend {
        RunnerBackend::StubEcho => Ok(echo_response(&spec.input)),
        RunnerBackend::StubSlow => {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(echo_response(&spec.input))
        }
        RunnerBackend::Llm(client) => {
            if self.pending_approvals.is_some() {
                let system = self
                    .prepare_system_prompt(&spec.input)
                    .await
                    .unwrap_or_default();
                self.run_agentic_loop(&id, client.as_ref(), system, &spec.input, sink)
                    .await
            } else {
                self.run_llm(&id, client.as_ref(), &spec.input, sink).await
            }
        }
    }
};

// Race generation against the cancel signal. On cancel, the generation future
// is dropped (Rust async cancellation aborts the in-flight LLM call).
let result: Option<Result<Message, TaskError>> = tokio::select! {
    r = generation => Some(r),
    _ = &mut rx_cancel => None,
};

// Always remove the cancel entry (success, failure, or cancel) to avoid leaks.
self.cancel_signals
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .remove(&id);

let now = chrono::Utc::now().to_rfc3339();
let result = match result {
    None => {
        // Cancelled: surface a Cancelled task carrying only the input so the
        // stream-return path in message/send terminates cleanly.
        self.set_state(&id, TaskState::Cancelled);
        return TaskOutcome::Cancelled(Task {
            id,
            state: TaskState::Cancelled,
            messages: vec![spec.input],
            created_at: now.clone(),
            completed_at: Some(now),
            error: None,
            usage: None,
        });
    }
    Some(r) => r,
};
```

Leave the existing `match result { Ok(reply) => … Err(err) => … }` block that follows it **unchanged** (it already builds `Completed` / `Failed` from `now`). Confirm `TaskState::Cancelled` exists (it is used in `start_async`); if `Task`/`TaskState` are imported, no new imports are needed.

- [ ] **Step 4: Run the cancel test — verify it passes**

Run: `cargo nextest run -p mur-agent-runtime run_sync_streaming_is_cancellable_by_id`
Expected: PASS (finishes in well under 2s).

- [ ] **Step 5: Run the full runtime suite + clippy — no regressions**

Run:
```bash
cargo nextest run -p mur-agent-runtime
cargo clippy -p mur-agent-runtime -- -D warnings
```
Expected: all green. (Existing `run_sync` / `message_send` tests still pass — the non-cancel path returns `Some(Ok/Err)` exactly as before.)

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): cancel in-flight generation in run_sync_inner via cancel_signals + select!"
```

---

## Task 2: Runtime — `message_send` reads the supplied id

Thread the client-supplied id from JSON-RPC params into `TaskSpec.task_id`.

**Files:**
- Modify: `mur-agent-runtime/src/protocol/methods/message_send.rs` (handler ~lines 60–71)
- Test: same file (`#[cfg(test)]` module — follow the existing handler-test pattern in `protocol/methods/`)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parses_top_level_task_id_into_spec() {
    let p = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": "hi" }] },
        "task_id": "task-from-client"
    });
    let task_id = p.get("task_id").and_then(|v| v.as_str()).map(|s| s.to_string());
    assert_eq!(task_id.as_deref(), Some("task-from-client"));

    let p2 = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": "hi" }] }
    });
    let none = p2.get("task_id").and_then(|v| v.as_str()).map(|s| s.to_string());
    assert_eq!(none, None);
}
```
(This pins the extraction expression. If the file already has a fuller handler harness — e.g. a fake runner asserting the spec — prefer extending that to assert `spec.task_id`; otherwise this expression test guards the parse.)

- [ ] **Step 2: Run test — verify it fails / is red**

Run: `cargo nextest run -p mur-agent-runtime parses_top_level_task_id_into_spec`
Expected: FAIL only if the production code path is what you assert against; for the expression-level test it passes once added — so instead drive the change by Step 3 first failing to compile if you extend the real harness. Either way, do not skip Step 3.

- [ ] **Step 3: Read the id in the handler**

In `message_send.rs`, where `context_task_id` is parsed and `TaskSpec` is built, add the top-level `task_id` read and field:
```rust
let context_task_id = p
    .get("context")
    .and_then(|c| c.get("task_id"))
    .and_then(|v| v.as_str())
    .map(|s| s.to_string());
let task_id = p
    .get("task_id")
    .and_then(|v| v.as_str())
    .map(|s| s.to_string());
let spec = TaskSpec {
    input: message,
    context_task_id,
    task_id,
};
```
Note `context.task_id` (multi-turn context, unchanged) and the new top-level `task_id` (this turn's cancellable id) are distinct — do not merge them.

- [ ] **Step 4: Run tests — verify pass**

Run: `cargo nextest run -p mur-agent-runtime message_send`
Expected: PASS (existing handler tests still pass; absent `task_id` → `None`, back-compatible).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/methods/message_send.rs
git commit -m "feat(runtime): message_send reads top-level task_id into TaskSpec"
```

---

## Task 3: Hub backend — in-flight registry + `agent_chat_send` threads the id

Add Tauri-managed state mapping `agent name → in-flight task id`, set when a send starts and cleared when it returns, and pass the id in `message/send` params.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/chat.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (`.manage(...)` block ~line 241)
- Test: `mur-hub-gui/src-tauri/src/chat.rs` (`#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test (registry semantics)**

Add to `chat.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_set_get_clear() {
        let reg = ChatRegistry::default();
        assert_eq!(reg.get("alice"), None);
        reg.set("alice", "task-1");
        assert_eq!(reg.get("alice").as_deref(), Some("task-1"));
        reg.clear("alice");
        assert_eq!(reg.get("alice"), None);
    }
}
```

- [ ] **Step 2: Run test — verify it fails to compile**

Run: `cargo test -p mur-hub-gui registry_set_get_clear` (this crate is workspace-excluded; build via its own manifest — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml registry_set_get_clear` if a bare `-p` does not resolve).
Expected: FAIL — `ChatRegistry` undefined.

- [ ] **Step 3: Add the registry type + managed-state wrapper**

At the top of `chat.rs` (after the imports), add:
```rust
use std::collections::HashMap;
use std::sync::Mutex;

/// Maps an agent name to the task id of its current in-flight chat turn, so a
/// Stop action can cancel by an id the Hub already holds. Single source of
/// truth for the cancel path.
#[derive(Default)]
pub struct ChatRegistry(Mutex<HashMap<String, String>>);

impl ChatRegistry {
    pub fn set(&self, agent: &str, task_id: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.to_string(), task_id.to_string());
    }
    pub fn get(&self, agent: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(agent)
            .cloned()
    }
    pub fn clear(&self, agent: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(agent);
    }
}

/// Tauri-managed wrapper.
#[derive(Default)]
pub struct ChatRegistryState(pub ChatRegistry);
```

- [ ] **Step 4: Run test — verify it passes**

Run: `cargo test -p mur-hub-gui registry_set_get_clear` (or the `--manifest-path` form).
Expected: PASS.

- [ ] **Step 5: Register the state in `lib.rs`**

In `lib.rs`, alongside the other `.manage(...)` calls (~line 241–246), add:
```rust
        .manage(chat::ChatRegistryState::default())
```

- [ ] **Step 6: Thread `taskId` through `agent_chat_send`**

Change the command signature and body in `chat.rs`. Add the `task_id` param, the registry handle, store-before-dial, pass `task_id` in params, and clear on completion:
```rust
#[tauri::command]
pub async fn agent_chat_send(
    app: AppHandle,
    registry: tauri::State<'_, ChatRegistryState>,
    name: String,
    text: String,
    task_id: String,
    context_task_id: Option<String>,
) -> Result<ChatReply, String> {
    let home = crate::mur_home_path();
    registry.0.set(&name, &task_id);

    let mut params = json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] },
        "task_id": task_id,
    });
    if let Some(tid) = context_task_id {
        params["context"] = json!({ "task_id": tid });
    }
    // … existing spawn_blocking dial block unchanged …
```
At **every** return point of the command (the `?` error paths and the final `Ok(...)`), clear the registry for `name`. The simplest correct approach: wrap the existing post-dial logic so the clear runs unconditionally. Replace the tail of the function with:
```rust
    let dialed = tokio::task::spawn_blocking(move || {
        // … existing closure returning Result<(Value, bool), _> …
    })
    .await
    .map_err(|e| format!("chat task panicked: {e}"))?;

    // Clear the in-flight id regardless of success/failure — the turn is over.
    registry.0.clear(&name);

    let (task, streamed) = dialed.map_err(|e| e.to_string())?;
    // … existing task_id / reply extraction and Ok(ChatReply { .. }) …
```
Keep returning `task_id` in `ChatReply` (it now equals the supplied id) so `ChatTab` still threads `context_task_id` for multi-turn.

- [ ] **Step 7: Build — verify it compiles**

Run: `cargo build -p mur-hub-gui` (or `--manifest-path mur-hub-gui/src-tauri/Cargo.toml`).
Expected: compiles. (The frontend now must pass `taskId`; Task 6 does that. Until then a manual chat would fail arg validation — acceptable mid-plan.)

- [ ] **Step 8: Commit**

```bash
git add mur-hub-gui/src-tauri/src/chat.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): in-flight chat registry; agent_chat_send threads client task id"
```

---

## Task 4: Hub backend — `agent_chat_cancel` command

Look up the agent's in-flight id and dial `tasks/cancel{id}` on a fresh connection; no-op if absent.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/chat.rs` (new command + test)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (`invoke_handler` list ~line 416)

- [ ] **Step 1: Write the failing test (no-op when absent)**

```rust
#[test]
fn cancel_lookup_is_none_when_absent() {
    let reg = ChatRegistry::default();
    // No id registered for "ghost" → cancel must no-op (None lookup).
    assert_eq!(reg.get("ghost"), None);
}
```
(The dial itself needs a running agent, so the unit test pins the no-op precondition; end-to-end cancel is exercised by Task 1's runtime test and manual verification in Task 7.)

- [ ] **Step 2: Run test — verify red/green harness**

Run: `cargo test -p mur-hub-gui cancel_lookup_is_none_when_absent`
Expected: PASS once compiled (guards the no-op contract). Proceed to implement the command.

- [ ] **Step 3: Add the command**

In `chat.rs`:
```rust
/// Cancel agent `name`'s in-flight chat turn by dialing `tasks/cancel` with the
/// id the Hub stored when the send began. No-ops if nothing is in flight.
#[tauri::command]
pub async fn agent_chat_cancel(
    registry: tauri::State<'_, ChatRegistryState>,
    name: String,
) -> Result<(), String> {
    let Some(task_id) = registry.0.get(&name) else {
        return Ok(()); // nothing in flight — nothing to cancel
    };
    let home = crate::mur_home_path();
    let params = json!({ "id": task_id });
    tokio::task::spawn_blocking(move || {
        // Separate connection so it doesn't fight the in-progress streaming read.
        match dial_method(&home, &name, "tasks/cancel", params, DialMode::RequireRunning) {
            Ok(_) => Ok(()),
            // The turn may have just finished — TaskNotFound / not-cancellable is benign.
            Err(e) if e.to_string().contains("not cancellable")
                || e.to_string().contains("not found")
                || e.to_string().contains("is not running") => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| format!("cancel task panicked: {e}"))?
}
```
Confirm the `tasks/cancel` params key the runtime expects is `id` (the `TasksCancelHandler` in `mur-agent-runtime/src/protocol/methods/tasks.rs`); match its exact param name. Confirm `DialMode::RequireRunning` is a real variant (`a2a_dial.rs` `enum DialMode`); if the closest variant is named differently, use that.

- [ ] **Step 4: Register the command in `lib.rs`**

In the `invoke_handler` list, after `chat::agent_chat_send,`:
```rust
            chat::agent_chat_cancel,
```

- [ ] **Step 5: Build + test**

Run:
```bash
cargo test -p mur-hub-gui cancel_lookup_is_none_when_absent
cargo build -p mur-hub-gui
```
Expected: PASS + compiles.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/chat.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): agent_chat_cancel command dials tasks/cancel by stored id"
```

---

## Task 5: Frontend — generate `taskId` per send

Send a fresh UUID with each turn so the backend registers a cancellable id. (Uses the WebView's built-in `crypto.randomUUID()` — no dependency.)

**Files:**
- Modify: `mur-hub-gui/ui/src/components/ChatTab.tsx` (`send()` ~lines 96–124)
- Test: `mur-hub-gui/ui/src/components/__tests__/chatTab.taskId.test.ts` (new; place beside existing Vitest tests — match the project's test dir convention found via `ls mur-hub-gui/ui/src/**/__tests__` or existing `*.test.ts`)

- [ ] **Step 1: Write the failing test (pure helper)**

Extract the id generation into a tiny pure helper so it is testable without a DOM. Create the test:
```ts
import { describe, it, expect } from "vitest";
import { newTaskId } from "../ChatTab";

describe("newTaskId", () => {
  it("produces a unique, non-empty id each call", () => {
    const a = newTaskId();
    const b = newTaskId();
    expect(a).toMatch(/^task-/);
    expect(a).not.toEqual(b);
  });
});
```

- [ ] **Step 2: Run test — verify it fails**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/__tests__/chatTab.taskId.test.ts`
Expected: FAIL — `newTaskId` is not exported.

- [ ] **Step 3: Add and use the helper**

In `ChatTab.tsx`, export the helper near the top:
```ts
export function newTaskId(): string {
  return `task-${crypto.randomUUID()}`;
}
```
In `send()`, generate the id and pass it (Tauri maps JS `taskId` → Rust `task_id`):
```ts
    setBusy(true);
    streamingRef.current = "";
    thinkingRef.current = "";
    setStreaming("");
    setThinking(null);
    const taskId = newTaskId();
    currentTaskIdRef.current = taskId; // used by Stop (Task 6)
    try {
      const res = await invoke<ChatReply>("agent_chat_send", {
        name: agentName,
        text,
        taskId,
        contextTaskId: taskIdRef.current,
      });
```
Add the ref near the other refs (`const currentTaskIdRef = useRef<string | null>(null);`). In the `finally` block, clear it: `currentTaskIdRef.current = null;`.

- [ ] **Step 4: Run test — verify it passes**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/__tests__/chatTab.taskId.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/ChatTab.tsx mur-hub-gui/ui/src/components/__tests__/chatTab.taskId.test.ts
git commit -m "feat(hub-ui): generate per-send task id for cancellation"
```

---

## Task 6: Frontend — Stop button + Esc + commit partial reply

Show **Stop** while `busy`; on Stop, call `agent_chat_cancel` + `voice` is out of scope (deferred), and **commit `streamingRef.current`** as an `agent` message tagged "stopped".

**Files:**
- Modify: `mur-hub-gui/ui/src/components/ChatTab.tsx`
- Test: `mur-hub-gui/ui/src/components/__tests__/chatTab.stop.test.ts` (new)

- [ ] **Step 1: Write the failing test (pure stop-commit logic)**

Extract the "build the stopped message from a partial buffer" decision into a pure helper and test it:
```ts
import { describe, it, expect } from "vitest";
import { buildStoppedMessage } from "../ChatTab";

describe("buildStoppedMessage", () => {
  it("commits the partial buffer tagged stopped", () => {
    expect(buildStoppedMessage("partial answer")).toEqual({
      role: "agent",
      text: "partial answer",
      stopped: true,
    });
  });
  it("still returns a stopped marker when the buffer is empty", () => {
    expect(buildStoppedMessage("")).toEqual({
      role: "agent",
      text: "",
      stopped: true,
    });
  });
});
```

- [ ] **Step 2: Run test — verify it fails**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/__tests__/chatTab.stop.test.ts`
Expected: FAIL — `buildStoppedMessage` not exported.

- [ ] **Step 3: Implement helper + stop handler + UI**

In `ChatTab.tsx`:

Extend the `ChatMsg` type with an optional flag (find the `role: "user" | "agent" | "error"` type ~line 14):
```ts
type ChatMsg = {
  role: "user" | "agent" | "error";
  text: string;
  stopped?: boolean;
};
```
Add the helper:
```ts
export function buildStoppedMessage(partial: string): ChatMsg {
  return { role: "agent", text: partial, stopped: true };
}
```
Add the stop handler:
```ts
  async function stop() {
    if (!busy) return;
    // Commit whatever streamed so far as the reply, tagged "stopped".
    const partial = streamingRef.current;
    setMessages((m) => [...m, buildStoppedMessage(partial)]);
    setStreaming(null);
    setThinking(null);
    streamingRef.current = "";
    thinkingRef.current = "";
    try {
      await invoke("agent_chat_cancel", { name: agentName });
    } catch {
      // benign: turn may have already finished
    }
    // Note: the in-flight agent_chat_send promise will still resolve/reject with
    // the Cancelled result; its result is ignored for content (we already
    // committed the partial). Guard the send() success path against double-append
    // by checking a `stoppedRef`.
  }
```
Add `const stoppedRef = useRef(false);`. In `send()`: set `stoppedRef.current = false;` at the start; in the success branch, **only** append the agent reply if `!stoppedRef.current`; in `stop()` set `stoppedRef.current = true;` before committing. This prevents the resolved `agent_chat_send` from appending a second (full or empty) agent bubble after Stop.

Extend `onKeyDown` for Esc:
```ts
  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Escape" && busy) {
      e.preventDefault();
      void stop();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }
```
Render the Stop control: where the Send button is rendered (`disabled={busy || !input.trim()}` ~line 210), show Stop while busy. Example:
```tsx
        {busy ? (
          <button type="button" onClick={() => void stop()} className="stop-btn">
            Stop
          </button>
        ) : (
          <button type="button" onClick={() => void send()} disabled={!input.trim()}>
            Send
          </button>
        )}
```
Match the surrounding JSX/className conventions actually present in the file. Optionally render a small "stopped" affordance for messages where `msg.stopped` is true in the message-list map.

- [ ] **Step 4: Run test — verify it passes**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/__tests__/chatTab.stop.test.ts`
Expected: PASS.

- [ ] **Step 5: Typecheck + full UI tests**

Run:
```bash
cd mur-hub-gui/ui && npx tsc --noEmit && npx vitest run
```
Expected: no type errors; all tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/ui/src/components/ChatTab.tsx mur-hub-gui/ui/src/components/__tests__/chatTab.stop.test.ts
git commit -m "feat(hub-ui): Stop button + Esc cancel; commit partial reply as stopped"
```

---

## Task 7: End-to-end verification + full builds

**Files:** none (verification only).

- [ ] **Step 1: Workspace build + targeted suites**

Run:
```bash
cargo nextest run -p mur-agent-runtime
cargo clippy -p mur-agent-runtime -- -D warnings
cargo build -p mur-hub-gui
cd mur-hub-gui/ui && npx tsc --noEmit && npx vitest run
```
Expected: all green.

- [ ] **Step 2: Manual smoke test (real agent)**

Launch the Hub against a running LLM-backed agent, send a prompt that produces a long reply, and click **Stop** (and separately, press **Esc**) mid-stream. Verify:
- the spinner stops immediately;
- the partial text already streamed remains, marked "stopped";
- token generation actually halts (watch the agent's logs / token usage — the reply does **not** keep growing server-side: the proof that this isn't a fake socket-close cancel);
- a subsequent send works and threads context normally.

- [ ] **Step 3: Edge cases**

- Press Stop with nothing in flight → no error, no stray bubble.
- Rapid double Stop → idempotent (second `tasks/cancel` swallowed).
- Stop an agent that just finished → benign no-op.

- [ ] **Step 4: Final commit (if any verification fixups were needed)**

```bash
git add -A && git commit -m "test(hub): interrupt/stop end-to-end verification fixups"
```

---

## Self-Review (against the spec)

**Spec coverage**

| Spec requirement | Task |
|---|---|
| `run_sync_inner` cancellation (register + `select!` + `Cancelled` + cleanup) — *Required Runtime Work / BLOCKER* | Task 1c |
| `TaskSpec.task_id: Option<String>` (additive) | Task 1a |
| `run_sync_streaming` id line honors supplied id | Task 1b |
| `message_send` reads `p["task_id"]` (back-compatible) | Task 2 |
| In-flight registry in `chat.rs` (set on send, cleared on completion) | Task 3 |
| `agent_chat_send` accepts `taskId`, passes `task_id` in params | Tasks 3 + 5 |
| `agent_chat_cancel` command (lookup → `tasks/cancel` on new connection, no-op if absent) | Task 4 |
| Client-supplied id mechanism (no `dial_message_streaming` signature change) | Tasks 3–5 (dial call unchanged) |
| `ChatTab` generates `taskId` per send | Task 5 |
| Stop button while `busy` + Esc | Task 6 |
| Partial-reply preservation (M1): commit `streamingRef` tagged "stopped", not the Cancelled body | Task 6 |
| Error handling: stop-before-id no-op; cancel-after-finish `TaskNotFound` swallowed; duplicate Stop idempotent | Tasks 4 + 6 + 7 Step 3 |
| Voice abort **deferred** (not in v1) | Out of scope — noted, no task |
| Testing matrix (runtime id/cancel, message_send parse, chat registry, ChatTab logic) | Tasks 1, 2, 3, 5, 6 |

**Deferred (intentionally no task):** `voice_abort` + `VoicePlayer` abort-handle plumbing; desktop STT/VAD; voice-activated barge-in; chat TTS; turn-taking state machine. These belong on the pet/companion surface per the spec's **Deferred** section.

**Coordination note (from spec):** `task_runner.rs`, `message_send.rs`, and `ChatTab.tsx` are also touched by in-flight Phase 3/4 and the multi-agent rail plan (`2026-06-08-hub-multiagent-conversation-rail.md`). All edits here are additive; execute this plan after those land (Task 0 re-anchors line numbers first).
