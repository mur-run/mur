# Agent Runtime + CLI — Mid-Turn Steering — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While a tool-using turn is running, the user types a course-correction and it's injected into the in-flight agentic loop on the next iteration — redirecting the agent **without killing the turn**.

**Architecture:** Reuse the existing per-task registry pattern. A streaming turn creates a steering mpsc channel; the **sender** is registered in a new `TaskRunner.steering` map keyed by `task_id` (mirrors `client_notifiers`); the **receiver** is threaded into `run_agentic_loop`, which `try_recv`s it at the race-free iteration boundary (after tool results, before the next LLM call) and appends the text to `history` as a user message. A new `turn/steer` A2A method looks up the sender by `task_id` and pushes the text. The cli, while streaming, sends `turn/steer` (with the live `current_task_id`) instead of rejecting the input. **Prerequisite:** the Anthropic message conversion must coalesce consecutive same-role messages, else the injected user message (after the user tool-results message) makes two consecutive user messages and Anthropic 400s.

**Tech Stack:** Rust (edition 2024), tokio mpsc, `async_trait` (MethodHandler), serde_json, crossterm (cli). No new dependency.

## Global Constraints

- **Independent feature** — branch from `main` (fetch first; release CI advances main). Touches `mur-agent-runtime` (most of it) + `mur-core/src/cmd/agent/cli/` (the send path). The cli change is a small edit to the streaming-reject branch in `submit()` + a system-note render, so it works regardless of the Glass Box cli PRs (a later rebase may have a tiny `submit()` conflict).
- **Rust edition 2024**; no hardcoded values (named `const` for channel capacity etc.).
- **Tests (toolchain cargo if rustup broken):** `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"`; plain `cargo test` (nextest absent). `mur-core` tests need `ORT_STRATEGY=download`; `mur-agent-runtime` does not.
- **Lint gate:** `cargo clippy -p mur-agent-runtime -- -D warnings` (and `-p mur-core` for the cli task) + `cargo fmt`.
- **Concurrency invariant (verified):** `history` is mutated only inside `run_agentic_loop`'s body at iteration boundaries; the LLM call uses `history.clone()`. A non-blocking `try_recv` + `history.push` at the boundary is race-free — no external task touches `history`.
- **Steering message shape:** injected as `RichMessage::Text { role: "user", content: "(steering) <text>" }` so the model sees a user interjection after the tool results.

---

### Task 1: Coalesce consecutive same-role messages in the Anthropic conversion

**Files:**
- Modify: `mur-agent-runtime/src/llm/anthropic.rs` (`rich_messages_to_anthropic`)
- Test: `mur-agent-runtime/src/llm/anthropic.rs` (inline)

**Interfaces:**
- Produces: `rich_messages_to_anthropic` now merges any two adjacent `convo` entries with the same `role` into one (content arrays concatenated). Same signature.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rich_to_anthropic_coalesces_consecutive_user_messages() {
    use crate::llm::{RichMessage, ToolResultEntry};
    // tool_results (user) immediately followed by an injected user steer.
    let msgs = vec![
        RichMessage::ToolResults {
            results: vec![ToolResultEntry {
                call_id: "c1".into(), content: "ok".into(), is_error: false,
            }],
        },
        RichMessage::Text { role: "user".into(), content: "(steering) use ripgrep".into() },
    ];
    let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
    // Must be ONE user message, not two (Anthropic forbids consecutive same-role).
    assert_eq!(convo.len(), 1, "consecutive user messages must coalesce: {convo:?}");
    assert_eq!(convo[0]["role"], "user");
    let content = convo[0]["content"].as_array().expect("content array");
    // tool_result block + the steering text block
    assert!(content.iter().any(|b| b["type"] == "tool_result"));
    assert!(content.iter().any(|b| b["type"] == "text" && b["text"].as_str() == Some("(steering) use ripgrep")));
}

#[test]
fn rich_to_anthropic_keeps_alternating_roles_separate() {
    use crate::llm::RichMessage;
    let msgs = vec![
        RichMessage::Text { role: "user".into(), content: "hi".into() },
        RichMessage::Text { role: "agent".into(), content: "hello".into() },
        RichMessage::Text { role: "user".into(), content: "bye".into() },
    ];
    let (_s, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(convo.len(), 3);
    assert_eq!(convo[1]["role"], "assistant");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime rich_to_anthropic_coalesces 2>&1 | tail -20`
Expected: FAIL — `convo.len()` is 2, not 1.

- [ ] **Step 3: Add a coalescing push helper + use it**

At the top of `rich_messages_to_anthropic`, add a closure/fn that pushes a `{role, content}` value, merging into the last entry if the role matches. Replace every `convo.push(json!({"role": …, "content": …}))` in the fn with a call to it. The helper (string content is normalized to a `[{type:text,text}]` block when merging is needed):

```rust
    fn push_coalesced(convo: &mut Vec<serde_json::Value>, role: &str, content: serde_json::Value) {
        // Normalize a message's content to an array of blocks.
        fn blocks(content: serde_json::Value) -> Vec<serde_json::Value> {
            match content {
                serde_json::Value::Array(a) => a,
                serde_json::Value::String(s) => vec![json!({"type":"text","text":s})],
                other => vec![other],
            }
        }
        if let Some(last) = convo.last_mut()
            && last["role"] == role
        {
            let mut merged = blocks(last["content"].take());
            merged.extend(blocks(content));
            last["content"] = json!(merged);
            return;
        }
        convo.push(json!({"role": role, "content": content}));
    }
```

Then replace the four `convo.push(json!({"role": r/…, "content": …}))` sites in the match arms with `push_coalesced(&mut convo, r_or_role, content_value)`. (For `RichMessage::Text` the content is the string; for `ToolUse`/`ToolResults`/`ImageText` it's the `parts` array — `push_coalesced` handles both.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-agent-runtime rich_to_anthropic 2>&1 | tail -20` (the 2 new + the existing `rich_messages_to_anthropic_*` tests).
Expected: PASS — and no regression in the existing conversion tests.

- [ ] **Step 5: Lint + commit**

Run: `cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`

```bash
git add mur-agent-runtime/src/llm/anthropic.rs
git commit -m "fix(runtime): coalesce consecutive same-role messages in Anthropic conversion (alternation safety for steering)"
```

---

### Task 2: Steering registry + channel plumbing

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (TaskRunner struct field + ctor init; `register_steering`/`unregister_steering`/`inject_steering`; `run_agentic_loop` signature gains a `steer_rx` param; `run_sync_streaming`/`run_sync_inner` thread it)
- Modify: `mur-agent-runtime/src/protocol/methods/message_send.rs` (create the steering channel, register the sender, pass the receiver, unregister)
- Test: `mur-agent-runtime/src/task_runner.rs` (inline — register→inject→the registered sender receives)

**Interfaces:**
- Produces: `TaskRunner.steering: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::mpsc::Sender<String>>>>`; `register_steering(&self, task_id, tx)`, `unregister_steering(&self, task_id)`, `inject_steering(&self, task_id, msg) -> Result<(), HandlerError>` (errors if no such task). `run_agentic_loop(..., steer_rx: Option<tokio::sync::mpsc::Receiver<String>>)`.
- Consumes: the existing `client_notifiers` pattern (mirror it exactly).

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn steering_register_inject_unregister() {
    let runner = TaskRunner::test_fixture(); // mirror an existing test ctor; if none, build minimally
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    runner.register_steering("t1", tx).await;
    runner.inject_steering("t1", "use ripgrep".into()).await.unwrap();
    assert_eq!(rx.recv().await.as_deref(), Some("use ripgrep"));
    // unknown task → error
    assert!(runner.inject_steering("nope", "x".into()).await.is_err());
    runner.unregister_steering("t1").await;
    assert!(runner.inject_steering("t1", "y".into()).await.is_err());
}
```

> If `TaskRunner` has no test constructor, add a minimal `#[cfg(test)]` one (or build the struct with defaults) sufficient to exercise the steering map. Grep `impl TaskRunner` / existing `#[tokio::test]` in the file for the pattern; mirror it. Keep it minimal.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime steering_register_inject 2>&1 | tail -20`
Expected: FAIL — field/methods missing.

- [ ] **Step 3: Add the field + methods** (mirror `client_notifiers`, task_runner.rs ~112-141 + ~386-400)

Struct field (next to `client_notifiers`):
```rust
    /// Per-turn steering channels keyed by task id. A running agentic loop
    /// holds the receiver; `turn/steer` pushes a user interjection here and the
    /// loop picks it up at the next iteration boundary.
    steering: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::mpsc::Sender<String>>>>,
```
Init in the constructor(s): `steering: Arc::new(tokio::sync::Mutex::new(HashMap::new())),` (find every `TaskRunner { … }` literal / `Self { … }` in the ctor and add it — grep `client_notifiers:` to find the same sites).

Methods (next to `register_client_notifier`):
```rust
    pub async fn register_steering(&self, task_id: &str, tx: tokio::sync::mpsc::Sender<String>) {
        self.steering.lock().await.insert(task_id.to_string(), tx);
    }
    pub async fn unregister_steering(&self, task_id: &str) {
        self.steering.lock().await.remove(task_id);
    }
    /// Push a steering message to the running task; errors if no such task.
    pub async fn inject_steering(
        &self,
        task_id: &str,
        msg: String,
    ) -> Result<(), crate::protocol::HandlerError> {
        let tx = self.steering.lock().await.get(task_id).cloned();
        match tx {
            Some(tx) => tx
                .send(msg)
                .await
                .map_err(|_| crate::protocol::HandlerError::TaskNotFound(task_id.to_string())),
            None => Err(crate::protocol::HandlerError::TaskNotFound(task_id.to_string())),
        }
    }
```
> Adjust the `HandlerError` path/variant to match the real type (grep `enum HandlerError`); use its "task not found"/"invalid" variant.

- [ ] **Step 4: Thread `steer_rx` into `run_agentic_loop`**

Add the param to `run_agentic_loop`'s signature (after `sink`):
```rust
        mut steer_rx: Option<tokio::sync::mpsc::Receiver<String>>,
```
(It's unused this task — prefix `_` is NOT used because Task 3 consumes it; add `#[allow(unused_mut)]`/`let _ = &steer_rx;` only if clippy complains this task; Task 3 removes any such stopgap. Cleanest: this task leaves the param named `steer_rx` and Task 3 uses it. If clippy flags unused, add a temporary `let _ = &mut steer_rx;` at the top and Task 3 deletes it.)

Thread it through the callers: in `run_sync_inner` (the call site that passes `sink` as the 6th arg to `run_agentic_loop`), pass the receiver as the new 7th arg; `run_sync_streaming` gains a `steer_rx` param (or `run_sync_inner` does) carried from `message_send.rs`.

- [ ] **Step 5: Create + register the channel in `message_send.rs`**

Where the turn registers its notifier (message_send.rs ~107-111) and after creating `turn_task_id`, create the steering channel and register the sender:
```rust
    let (steer_tx, steer_rx) = tokio::sync::mpsc::channel::<String>(STEER_CAP);
    if let Some(tid) = &turn_task_id {
        self.runner.register_steering(tid, steer_tx).await;
    }
```
Pass `steer_rx` (as `Some(steer_rx)`) into `run_sync_streaming`/`run_sync_inner` → `run_agentic_loop`. After the turn completes (next to `unregister_client_notifier`, ~140-142):
```rust
    if let Some(tid) = &turn_task_id {
        self.runner.unregister_steering(tid).await;
    }
```
Add `const STEER_CAP: usize = 16;` near the other consts in that file.

- [ ] **Step 6: Run test + build**

Run: `cargo test -p mur-agent-runtime steering_register_inject && cargo check -p mur-agent-runtime`
Expected: PASS + clean (the loop doesn't use `steer_rx` yet — Task 3).

- [ ] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs mur-agent-runtime/src/protocol/methods/message_send.rs
git commit -m "feat(runtime): per-task steering registry + channel plumbing (registered, not yet consumed)"
```

---

### Task 3: Inject steering messages at the loop boundary

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (`run_agentic_loop` — `try_recv` `steer_rx` after tool results, before the next iteration)

**Interfaces:**
- Consumes: `steer_rx` (Task 2), `history` (the loop's conversation `Vec<RichMessage>`).

- [ ] **Step 1: Add the injection at the iteration boundary**

After the loop pushes `RichMessage::ToolResults { results }` (task_runner.rs ~1344) and before `iteration += 1;`, drain any pending steering messages into `history`:
```rust
        // Mid-turn steering: pick up any user interjection sent via turn/steer
        // since the last LLM call and append it before the next iteration.
        // Race-free: history is mutated only here; try_recv never blocks.
        if let Some(rx) = steer_rx.as_mut() {
            while let Ok(msg) = rx.try_recv() {
                history.push(RichMessage::Text {
                    role: "user".into(),
                    content: format!("(steering) {msg}"),
                });
            }
        }
```

> Place it ONLY at the post-tool-results boundary (not after the `MaxTokens` `continue`, which already injects guidance, and not after the early-exit `return` — the turn is ending there). One injection point keeps causal order (tool → result → steer → next call). The Task-1 coalescing makes the resulting consecutive-user messages valid.

- [ ] **Step 2: Build + lint**

Run: `cargo check -p mur-agent-runtime && cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`
Expected: clean (remove any Task-2 stopgap `let _ = &mut steer_rx;` — it's now genuinely used).

- [ ] **Step 3: Full suite (no regressions)**

Run: `cargo test -p mur-agent-runtime 2>&1 | grep -E "test result|error\[|FAILED" | tail`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): agentic loop picks up steering messages at the iteration boundary"
```

---

### Task 4: `turn/steer` A2A method

**Files:**
- Create: `mur-agent-runtime/src/protocol/methods/turn.rs` (`TurnSteerHandler`)
- Modify: the methods module (`protocol/methods/mod.rs`) + the handler-registration site (where `tasks/cancel` etc. are inserted into the dispatcher's `methods` map)
- Test: `turn.rs` (inline — handler params validation)

**Interfaces:**
- Produces: `turn/steer` method — params `{ task_id: String, message: String }` → calls `runner.inject_steering(task_id, message)` → `{"task_id", "steered": true}` or a task-not-found error.

- [ ] **Step 1: Write the handler** (mirror `TasksCancelHandler`, the gather's template)

```rust
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::protocol::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::TaskRunner;

pub struct TurnSteerHandler {
    pub runner: Arc<TaskRunner>,
}

#[async_trait]
impl MethodHandler for TurnSteerHandler {
    async fn handle(&self, params: Option<Value>, _ctx: &RequestContext) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let task_id = p.get("task_id").and_then(Value::as_str)
            .ok_or_else(|| HandlerError::InvalidParams("missing task_id".into()))?;
        let message = p.get("message").and_then(Value::as_str)
            .ok_or_else(|| HandlerError::InvalidParams("missing message".into()))?;
        if message.trim().is_empty() {
            return Err(HandlerError::InvalidParams("empty steering message".into()));
        }
        self.runner.inject_steering(task_id, message.to_string()).await?;
        Ok(json!({ "task_id": task_id, "steered": true }))
    }
}
```
> Match the real `MethodHandler` trait + `HandlerError` variants + `RequestContext` (grep the `tasks/cancel` handler file and copy its imports/shape exactly).

- [ ] **Step 2: Register it** — find where handlers are inserted into the dispatcher's `methods` map (grep `"tasks/cancel"` / `"tool/hitl_respond"` registration), and add:
```rust
    methods.insert("turn/steer".into(), Box::new(TurnSteerHandler { runner: runner.clone() }));
```
Add `mod turn; pub use turn::TurnSteerHandler;` (or match the module's re-export style) in `protocol/methods/mod.rs`.

- [ ] **Step 3: Test the param validation**

```rust
#[tokio::test]
async fn turn_steer_rejects_missing_and_empty() {
    let runner = std::sync::Arc::new(crate::task_runner::TaskRunner::test_fixture());
    let h = TurnSteerHandler { runner };
    let ctx = /* a minimal RequestContext — mirror an existing handler test */;
    assert!(h.handle(None, &ctx).await.is_err());
    assert!(h.handle(Some(serde_json::json!({"task_id":"t"})), &ctx).await.is_err()); // no message
    assert!(h.handle(Some(serde_json::json!({"task_id":"t","message":"  "})), &ctx).await.is_err()); // empty
    // unknown task → TaskNotFound (no steering registered)
    assert!(h.handle(Some(serde_json::json!({"task_id":"t","message":"go"})), &ctx).await.is_err());
}
```
> Build `RequestContext` the way the existing method-handler tests do (grep a `#[tokio::test]` in `protocol/methods/`). If constructing it is heavy, test `inject_steering` directly (Task 2 covers the runner) and keep this test to the param-validation branches via a registered task.

- [ ] **Step 4: Run test + build + commit**

Run: `cargo test -p mur-agent-runtime turn_steer && cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`

```bash
git add mur-agent-runtime/src/protocol/
git commit -m "feat(runtime): turn/steer A2A method injects a mid-turn steering message"
```

---

### Task 5: cli — steer while streaming

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/stream.rs` (add `steer_turn` dial helper, mirror `cancel_task`)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`submit()` — while streaming, send `turn/steer` instead of rejecting; render a note)
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (a `push_system`/render for the steer; the input-box hint)
- Test: manual (network dial); the steer-vs-reject branch logic can have a small unit assertion if feasible.

**Interfaces:**
- Produces: `stream::steer_turn(home, agent, task_id, message) -> Result<()>` (dials `turn/steer`).
- Consumes: `App.current_task_id` (the live turn id), `dial_method`/`DialMode` (existing).

- [ ] **Step 1: Add the dial helper** (in stream.rs, mirror `cancel_task`/`respond_hitl`)

```rust
/// Inject a steering message into the in-flight turn on a fresh connection.
pub async fn steer_turn(
    home: PathBuf,
    agent: String,
    task_id: String,
    message: String,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        dial_method(
            &home,
            &agent,
            "turn/steer",
            json!({ "task_id": task_id, "message": message }),
            DialMode::RequireRunning,
        )
    })
    .await??;
    Ok(())
}
```

- [ ] **Step 2: Steer instead of reject in `submit()`**

Replace the streaming-reject branch in `submit()` (mod.rs ~549-554):
```rust
    if app.streaming {
        app.push_system("still generating — press Ctrl+C to cancel first");
        return;
    }
```
with a steer when there's a live task id:
```rust
    if app.streaming {
        if let Some(task_id) = app.current_task_id.clone() {
            let (h, a) = (app.home.clone(), app.agent.clone());
            let (msg, t) = (trimmed.clone(), tx.clone());
            app.push_system(format!("↗ steering: {trimmed}"));
            app.input.select_all();
            app.input.cut(); // clear the input (matches the normal send clear)
            tokio::spawn(async move {
                if let Err(e) = stream::steer_turn(h, a, task_id, msg).await {
                    let _ = t.send(StreamMsg::Note(format!("steer failed: {e:#}"))).await;
                }
            });
        } else {
            app.push_system("still generating — press Ctrl+C to cancel first");
        }
        return;
    }
```
> Use the cli's actual input-clear idiom (grep how the normal post-send clear works — e.g. `app.input = TextArea::default()` or `select_all()+cut()`); match it so the input box empties after steering. `current_task_id` is the live turn id (set in `begin_user_turn`, cleared on finish).

- [ ] **Step 3: Discoverability hint**

In the input-box title string (app.rs — the same place P2b added `· Ctrl+O transcript`, or the streaming-state status), add a hint that typing while generating steers, e.g. status when streaming: `generating… (type to steer · Ctrl+C cancel)`. Find the streaming status string in `ui.rs::render_status` and append `· type to steer`.

- [ ] **Step 4: Build + lint**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Expected: clean. Confirm cli tests still pass: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/
git commit -m "feat(cli): type while generating to steer the in-flight turn (turn/steer)"
```

---

## Manual verification (after all tasks)

1. Build runtime + cli: `cargo build --release -p mur-agent-runtime -p mur-core`.
2. Restart a tool-using agent onto the new runtime; `./target/release/mur agent cli <agent>`.
3. Ask for a multi-step task ("read these 5 files and refactor X"). While it's running tools, type **"actually, just summarize them instead"** + Enter. Confirm:
   - the turn does NOT cancel; a `↗ steering:` note appears;
   - on the next loop iteration the agent **changes course** (the steering message is in its context);
   - the final answer reflects the redirect.
4. Sanity: steering with no live task (not streaming) still does a normal send; `Ctrl+C` still cancels.

## Out of scope

- Steering the non-agentic (`run_llm`, no-tool) path — there are no iterations to inject between; the turn is a single call. (Type-to-steer only applies to tool turns.)
- Multiple concurrent turns per agent (one `current_task_id` at a time in the cli).

## Self-Review (completed)

- **Spec coverage:** alternation safety (T1), registry+plumbing (T2), loop injection (T3), turn/steer method (T4), cli steer (T5). ✔
- **Placeholder scan:** none — code in every step; the few `grep to confirm X` notes are anchors against real code (HandlerError variants, RequestContext ctor, input-clear idiom), not logic placeholders. ✔
- **Type consistency:** `register/unregister/inject_steering` + `steering: Mutex<HashMap<String, Sender<String>>>` (T2) consumed by T3 (`steer_rx`) and T4 (`inject_steering`); `steer_turn` (T5) dials the `turn/steer` of T4. ✔
