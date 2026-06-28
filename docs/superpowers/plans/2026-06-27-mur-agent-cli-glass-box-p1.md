# mur agent cli — Glass Box P1 (Step Transparency Spine) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every tool the agent runs a visible step in `mur agent cli` — name, args, result, duration — plus keep reasoning on screen and add a token/cost/context/timer footer.

**Architecture:** The runtime (`mur-agent-runtime`) emits two new JSON-RPC notifications (`step/started`, `step/completed`) around each tool execution. The cli's stream bridge parses them into new `StreamMsg` variants; the transcript gains tool-card entries (a new `StepCard` rendered inline, expanded-by-default but bounded). Reasoning stops being erased on turn-finish. A new `footer` module computes tokens/cost/context from `Task.usage` + the agent's `models.yaml` pricing.

**Tech Stack:** Rust (edition 2024), ratatui + crossterm (TUI), serde_json (A2A JSON-RPC), tokio (async), `mur_common::model::ModelRegistry` (pricing).

## Global Constraints

- **Rust edition 2024** — `let` chains stable.
- **No hardcoded values** — every cap/threshold/width is a named `const` (Mandatory Rule 1).
- **Brand "MUR" uppercase** in any user-facing string; internal `name`/dirs stay lowercase.
- **Single source file ≤ 800 lines** — `mod.rs` (857) and `app.rs` are already large; add new code in new modules (`cli/step.rs`, `cli/footer.rs`), not by growing `mod.rs`.
- **Tests run under nextest with `ORT_STRATEGY=download`** for `mur-core` (plain `cargo test --workspace` and `cargo build --workspace` fail on the onnxruntime link). Use `cargo check -p <crate>` and `cargo nextest run -p <crate>` per task.
- **Lint gate:** `cargo clippy -p <crate> -- -D warnings` and `cargo fmt` must pass.
- **mur-core MUST NOT depend on mur-agent-runtime** (forbidden dep edge). Pricing resolution is replicated in mur-core from `mur_common` + the agent profile.
- **Backward-compat:** an older runtime that never sends `step/*` must still work (no cards, no panic) — see Task 10.

---

### Task 1: Runtime emits `step/started` + `step/completed` (+ `context_tokens`)

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (`handle_tool_call`, ~1008–1125; the usage JSON closure, ~672–682)
- Test: `mur-agent-runtime/src/task_runner.rs` (inline `#[cfg(test)] mod step_tests`)

**Interfaces:**
- Produces (wire): notification `step/started` `{ jsonrpc, method, params: { step_id, task_id, kind:"tool", name, args } }` and `step/completed` `{ ..., params: { step_id, task_id, ok, output, truncated, full_len, error, duration_ms } }`, sent on the existing `tokio::sync::mpsc::Sender<serde_json::Value>` notifier.
- Produces (wire): the per-turn usage JSON gains `context_tokens` (u64) = the input-token count of the **last** LLM request in the loop (≈ current context fill, unlike cumulative `input_tokens`).
- Consumes: `call.tool_name: String`, `call.input: serde_json::Value`, `output: String`, `is_error: bool`, `task_id: &str` (all already in scope in `handle_tool_call`).

- [ ] **Step 1: Write the failing test** (pure helpers — the wiring is verified by build + manual run; the logic risk is the JSON shape + output cap)

Add at the bottom of `task_runner.rs`:

```rust
#[cfg(test)]
mod step_tests {
    use super::{cap_step_output, step_notification, STEP_MAX_BYTES};

    #[test]
    fn notification_has_jsonrpc_envelope_and_method() {
        let n = step_notification("step/started", serde_json::json!({ "step_id": "s1" }));
        assert_eq!(n["jsonrpc"], "2.0");
        assert_eq!(n["method"], "step/started");
        assert_eq!(n["params"]["step_id"], "s1");
    }

    #[test]
    fn cap_truncates_oversized_output_on_char_boundary() {
        let big = "é".repeat(STEP_MAX_BYTES); // 2 bytes/char → over the cap
        let (out, truncated, full_len) = cap_step_output(&big);
        assert!(truncated);
        assert_eq!(full_len, big.len());
        assert!(out.len() <= STEP_MAX_BYTES + "\n[truncated]".len());
        assert!(out.is_char_boundary(out.len())); // never split a char
    }

    #[test]
    fn cap_passes_small_output_through() {
        let (out, truncated, full_len) = cap_step_output("hello");
        assert!(!truncated);
        assert_eq!(out, "hello");
        assert_eq!(full_len, 5);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-agent-runtime step_tests 2>&1 | tail -20`
Expected: FAIL — `cannot find function cap_step_output` / `step_notification` / `STEP_MAX_BYTES`.

- [ ] **Step 3: Add the helpers** (near the top of `task_runner.rs`, after the `use` block)

```rust
/// Max bytes of a tool's output forwarded inline in a `step/completed`
/// notification. Larger output is truncated with a marker; full recovery is a
/// later phase (reads from the final task JSON).
pub(crate) const STEP_MAX_BYTES: usize = 8 * 1024;

/// Wrap step params in the JSON-RPC notification envelope the streaming socket
/// expects (mirrors the existing `tool/approval_needed` shape).
pub(crate) fn step_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// Cap tool output to `STEP_MAX_BYTES` on a char boundary. Returns
/// `(capped, was_truncated, full_byte_len)`.
pub(crate) fn cap_step_output(output: &str) -> (String, bool, usize) {
    let full_len = output.len();
    if full_len <= STEP_MAX_BYTES {
        return (output.to_string(), false, full_len);
    }
    let mut cut = STEP_MAX_BYTES;
    while !output.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut s = output[..cut].to_string();
    s.push_str("\n[truncated]");
    (s, true, full_len)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-agent-runtime step_tests 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 5: Wire the emissions into `handle_tool_call`** (both the `ToolPolicy::Allow` arm ~1044 and the `ToolPolicy::Ask` arm ~1060)

Resolve the notifier once near the top of `handle_tool_call` (after the unknown-tool guard, before the policy match) so both arms can use it:

```rust
        // Step events go to the same connection as approvals (route by task id,
        // fall back to the baked notifier). None → nobody is listening; skip.
        let step_notifier = {
            let routed = self.client_notifiers.lock().await.get(task_id).cloned();
            routed.or_else(|| self.notifier.clone())
        };
        let step_id = uuid::Uuid::now_v7().to_string();
```

Add a small async closure helper just below it (captures `task_id`, `call`, `step_id`, `step_notifier`):

```rust
        macro_rules! emit_step {
            (started) => {
                if let Some(n) = &step_notifier {
                    let _ = n
                        .send(step_notification(
                            "step/started",
                            serde_json::json!({
                                "step_id": step_id, "task_id": task_id, "kind": "tool",
                                "name": call.tool_name, "args": call.input,
                            }),
                        ))
                        .await;
                }
            };
            (completed, $output:expr, $is_error:expr, $dur_ms:expr) => {
                if let Some(n) = &step_notifier {
                    let (out, truncated, full_len) = cap_step_output($output);
                    let _ = n
                        .send(step_notification(
                            "step/completed",
                            serde_json::json!({
                                "step_id": step_id, "task_id": task_id,
                                "ok": !$is_error, "output": out,
                                "truncated": truncated, "full_len": full_len,
                                "error": if $is_error { Some($output) } else { None },
                                "duration_ms": $dur_ms,
                            }),
                        ))
                        .await;
                }
            };
        }
```

In the **Allow arm**, replace the execute block:

```rust
                ToolPolicy::Allow => {
                    let tool = tool.unwrap();
                    emit_step!(started);
                    let t0 = std::time::Instant::now();
                    let (output, is_error) = match tool.execute(call.input.clone()).await {
                        Ok(out) => (out, false),
                        Err(e) => (format!("tool error: {e}"), true),
                    };
                    emit_step!(completed, &output, is_error, t0.elapsed().as_millis() as u64);
                    return Ok(ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: output,
                        is_error,
                    });
                }
```

In the **Ask arm** (the `// 2. Execute the tool` block after the policy match), bracket the execute the same way:

```rust
        let tool = tool.unwrap();
        emit_step!(started);
        let t0 = std::time::Instant::now();
        let (output, is_error) = match tool.execute(call.input.clone()).await {
            Ok(out) => (out, false),
            Err(e) => (format!("tool error: {e}"), true),
        };
        emit_step!(completed, &output, is_error, t0.elapsed().as_millis() as u64);
```

- [ ] **Step 6: Add `context_tokens` to the usage JSON**

Grep for where input tokens are recorded per LLM call:

Run: `rg -n "cumulative_input_tokens" mur-agent-runtime/src/task_runner.rs`

Find the line that updates `cumulative_input_tokens` after each LLM call (a `.fetch_add(...)`). Add a sibling atomic that stores (not adds) the **last** call's input count. Near the struct's other atomics (grep `cumulative_input_tokens` in the struct def), add:

```rust
    /// Input tokens of the most recent LLM request — ≈ current context fill
    /// (cumulative_input_tokens over-counts across tool-loop iterations).
    last_input_tokens: std::sync::atomic::AtomicU64,
```

Initialize it `AtomicU64::new(0)` wherever the struct is constructed. At the `fetch_add` site, also:

```rust
    self.last_input_tokens
        .store(this_call_input, std::sync::atomic::Ordering::Relaxed);
```

(`this_call_input` is whatever local holds the current call's input token count at the `fetch_add`.) Then in the `token_usage` closure (~672–682), add the field:

```rust
    serde_json::json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "context_tokens": self.last_input_tokens.load(std::sync::atomic::Ordering::Relaxed),
    })
```

> If `last_input_tokens` proves entangled (no clean single-call input count at the `fetch_add`), STOP this step, drop `context_tokens`, and note that the footer context bar moves to P2. The rest of P1 is unaffected.

- [ ] **Step 7: Verify build + lint, then commit**

Run: `cargo check -p mur-agent-runtime && cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`
Expected: clean.

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): emit step/started + step/completed + context_tokens for cli step view"
```

---

### Task 2: cli stream bridge parses `step/*` into `StreamMsg`

**Files:**
- Modify: `mur-core/src/a2a_dial.rs` (`dial_message_streaming` ~204–273; add `StepEvent`, `parse_step`, `on_step` param, dispatch arms)
- Modify: `mur-core/src/cmd/agent/cli/stream.rs` (`StreamMsg` enum ~26–46; `task_id()` ~56; `spawn_stream` closure ~197–225)
- Test: `mur-core/src/a2a_dial.rs` (inline `#[cfg(test)] mod step_parse_tests`)

**Interfaces:**
- Produces: `a2a_dial::StepEvent::{Started{step_id,task_id,name,args}, Completed{step_id,task_id,ok,output,truncated,full_len,error,duration_ms}}`.
- Produces: `dial_message_streaming(home, agent_name, params, on_delta, on_hitl, on_step)` — one new `on_step: impl FnMut(StepEvent)` param appended.
- Produces: `StreamMsg::StepStarted{task_id,step_id,name,args}` and `StreamMsg::StepCompleted{task_id,step_id,ok,output,truncated,full_len,error,duration_ms}`.
- Consumes: Task 1's wire notifications.

- [ ] **Step 1: Write the failing test** (in `a2a_dial.rs`)

```rust
#[cfg(test)]
mod step_parse_tests {
    use super::{parse_step, StepEvent};

    #[test]
    fn parses_started() {
        let p = serde_json::json!({
            "step_id": "s1", "task_id": "t1", "name": "edit",
            "args": { "path": "a.rs" }
        });
        match parse_step(&p, false) {
            StepEvent::Started { step_id, name, args, .. } => {
                assert_eq!(step_id, "s1");
                assert_eq!(name, "edit");
                assert_eq!(args["path"], "a.rs");
            }
            _ => panic!("expected Started"),
        }
    }

    #[test]
    fn parses_completed_with_defaults() {
        let p = serde_json::json!({
            "step_id": "s1", "task_id": "t1", "ok": false,
            "output": "boom", "truncated": false, "full_len": 4,
            "error": "exit 1", "duration_ms": 12
        });
        match parse_step(&p, true) {
            StepEvent::Completed { ok, output, error, duration_ms, .. } => {
                assert!(!ok);
                assert_eq!(output, "boom");
                assert_eq!(error.as_deref(), Some("exit 1"));
                assert_eq!(duration_ms, 12);
            }
            _ => panic!("expected Completed"),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core step_parse_tests 2>&1 | tail -20`
Expected: FAIL — `cannot find function parse_step` / `StepEvent`.

- [ ] **Step 3: Add `StepEvent` + `parse_step` to `a2a_dial.rs`** (above `dial_message_streaming`)

```rust
/// A per-tool step event streamed from the runtime during a turn.
#[derive(Debug, Clone)]
pub enum StepEvent {
    Started {
        step_id: String,
        task_id: String,
        name: String,
        args: serde_json::Value,
    },
    Completed {
        step_id: String,
        task_id: String,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
    },
}

/// Parse the `params` of a `step/started` (`completed = false`) or
/// `step/completed` (`completed = true`) notification.
pub fn parse_step(p: &serde_json::Value, completed: bool) -> StepEvent {
    use serde_json::Value;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let step_id = s("step_id");
    let task_id = s("task_id");
    if completed {
        StepEvent::Completed {
            step_id,
            task_id,
            ok: p.get("ok").and_then(Value::as_bool).unwrap_or(true),
            output: s("output"),
            truncated: p.get("truncated").and_then(Value::as_bool).unwrap_or(false),
            full_len: p.get("full_len").and_then(Value::as_u64).unwrap_or(0) as usize,
            error: p.get("error").and_then(Value::as_str).map(str::to_string),
            duration_ms: p.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
        }
    } else {
        StepEvent::Started {
            step_id,
            task_id,
            name: s("name"),
            args: p.get("args").cloned().unwrap_or(Value::Null),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core step_parse_tests 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Add the `on_step` param + dispatch arms to `dial_message_streaming`**

Change the signature (append one param):

```rust
pub fn dial_message_streaming(
    home: &Path,
    agent_name: &str,
    params: Value,
    mut on_delta: impl FnMut(&str, bool, &str),
    mut on_hitl: impl FnMut(Value),
    mut on_step: impl FnMut(StepEvent),
) -> Result<Value>
```

In the dispatch loop, **after** the `tool/approval_needed` arm and **before** the `if v.get("id") == Some(&request_id)` arm, add:

```rust
            if v.get("method").and_then(Value::as_str) == Some("step/started") {
                if let Some(p) = v.get("params") {
                    on_step(parse_step(p, false));
                }
                continue;
            }
            if v.get("method").and_then(Value::as_str) == Some("step/completed") {
                if let Some(p) = v.get("params") {
                    on_step(parse_step(p, true));
                }
                continue;
            }
```

- [ ] **Step 6: Update every `dial_message_streaming` caller**

Run: `rg -n "dial_message_streaming\(" mur-core/src`
For each caller that doesn't need steps, pass a no-op `|_| {}` as the final arg. For `stream.rs::spawn_stream`, do Step 7 instead.

- [ ] **Step 7: Add the `StreamMsg` variants + `task_id()` arms + forward from `spawn_stream`**

In `stream.rs`, extend the `StreamMsg` enum:

```rust
    /// A tool call started running (name + args).
    StepStarted {
        task_id: String,
        step_id: String,
        name: String,
        args: Value,
    },
    /// A tool call finished (result/error + duration).
    StepCompleted {
        task_id: String,
        step_id: String,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
    },
```

In `task_id()`, add the two variants to the `Some(task_id)` side (they carry a `task_id`, so they must be filtered by the current-turn guard):

```rust
            StreamMsg::StepStarted { task_id, .. } | StreamMsg::StepCompleted { task_id, .. } => {
                Some(task_id)
            }
```

In `spawn_stream`, pass an `on_step` closure to `dial_message_streaming` that maps `StepEvent` → `StreamMsg` and sends on `tx`:

```rust
            |step| {
                let msg = match step {
                    crate::a2a_dial::StepEvent::Started { step_id, task_id, name, args } => {
                        StreamMsg::StepStarted { task_id, step_id, name, args }
                    }
                    crate::a2a_dial::StepEvent::Completed {
                        step_id, task_id, ok, output, truncated, full_len, error, duration_ms,
                    } => StreamMsg::StepCompleted {
                        task_id, step_id, ok, output, truncated, full_len, error, duration_ms,
                    },
                };
                let _ = tx.blocking_send(msg);
            },
```

- [ ] **Step 8: Verify, then commit**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core step_parse_tests && cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Expected: clean.

```bash
git add mur-core/src/a2a_dial.rs mur-core/src/cmd/agent/cli/stream.rs
git commit -m "feat(cli): parse step/started + step/completed into StreamMsg"
```

---

### Task 3: `StepCard` model (`cli/step.rs`)

**Files:**
- Create: `mur-core/src/cmd/agent/cli/step.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (add `mod step;` / `pub mod step;` next to the other `mod` decls)
- Test: `mur-core/src/cmd/agent/cli/step.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `StepCard { id, name, args, state, output, truncated, full_len, error, started, duration_ms }`, `StepState::{Running, Done, Error}`.
- Produces: `StepCard::new(id, name, args) -> Self`, `StepCard::complete(&mut self, ok, output, truncated, full_len, error, duration_ms)`, `StepCard::summary(&self) -> String`, `StepCard::glyph(&self) -> &'static str`.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test** (create the file with test first)

```rust
//! A single tool-call step rendered inline in the cli transcript.

#[cfg(test)]
mod tests {
    use super::{StepCard, StepState};

    fn card() -> StepCard {
        StepCard::new("s1".into(), "read".into(), serde_json::json!({ "path": "auth.rs" }))
    }

    #[test]
    fn new_card_is_running() {
        let c = card();
        assert_eq!(c.state, StepState::Running);
        assert_eq!(c.glyph(), "◐");
    }

    #[test]
    fn complete_ok_sets_done_and_glyph() {
        let mut c = card();
        c.complete(true, "412 lines".into(), false, 9, None, 8);
        assert_eq!(c.state, StepState::Done);
        assert_eq!(c.glyph(), "✔");
        assert_eq!(c.duration_ms, Some(8));
    }

    #[test]
    fn complete_err_sets_error_and_keeps_message() {
        let mut c = card();
        c.complete(false, "boom".into(), false, 4, Some("exit 1".into()), 3);
        assert_eq!(c.state, StepState::Error);
        assert_eq!(c.glyph(), "✗");
        assert_eq!(c.error.as_deref(), Some("exit 1"));
    }

    #[test]
    fn summary_is_one_line_name_plus_arg_hint() {
        let c = card();
        let s = c.summary();
        assert!(s.contains("read"));
        assert!(!s.contains('\n'));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli::step 2>&1 | tail -20`
Expected: FAIL — module/types not found (after adding `mod step;` in Step 4 it compiles; before that the file isn't included). First add `mod step;` to `mod.rs`, then re-run to get a real compile failure on missing types.

- [ ] **Step 3: Implement `step.rs`** (above the test module)

```rust
use std::time::Instant;

/// Lifecycle of a tool-call step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Error,
}

/// Max args lines shown inside an expanded card (mirrors the old HITL modal cap).
pub const ARGS_MAX_LINES: usize = 12;

/// One tool call, shown inline in the transcript.
#[derive(Debug, Clone)]
pub struct StepCard {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
    pub state: StepState,
    pub output: String,
    pub truncated: bool,
    pub full_len: usize,
    pub error: Option<String>,
    pub started: Instant,
    pub duration_ms: Option<u64>,
}

impl StepCard {
    pub fn new(id: String, name: String, args: serde_json::Value) -> Self {
        Self {
            id,
            name,
            args,
            state: StepState::Running,
            output: String::new(),
            truncated: false,
            full_len: 0,
            error: None,
            started: Instant::now(),
            duration_ms: None,
        }
    }

    pub fn complete(
        &mut self,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
    ) {
        self.state = if ok { StepState::Done } else { StepState::Error };
        self.output = output;
        self.truncated = truncated;
        self.full_len = full_len;
        self.error = error;
        self.duration_ms = Some(duration_ms);
    }

    pub fn glyph(&self) -> &'static str {
        match self.state {
            StepState::Running => "◐",
            StepState::Done => "✔",
            StepState::Error => "✗",
        }
    }

    /// One-line header summary: a compact hint of the first scalar arg, if any.
    pub fn summary(&self) -> String {
        let hint = self
            .args
            .as_object()
            .and_then(|m| m.values().find_map(|v| v.as_str()))
            .unwrap_or("");
        if hint.is_empty() {
            self.name.clone()
        } else {
            format!("{}  {}", self.name, hint)
        }
    }
}
```

- [ ] **Step 4: Register the module** in `mod.rs` (next to `mod app;`, `mod stream;`, etc.)

```rust
mod step;
```

- [ ] **Step 5: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli::step 2>&1 | tail -20`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/step.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): StepCard model for inline tool-call steps"
```

---

### Task 4: Transcript holds tool cards + segment interleaving (`app.rs`)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`ChatMsg` struct + ctors ~35–70; `append_delta` ~329–341; `finish_agent_turn` ~347–366; add `push_step_started` / `update_step_completed`)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`handle_stream` ~749–777: route the two new `StreamMsg` variants)
- Test: `mur-core/src/cmd/agent/cli/app.rs` (inline `#[cfg(test)] mod step_app_tests`)

**Interfaces:**
- Produces: `ChatMsg.step: Option<StepCard>`; `ChatMsg::tool(StepCard) -> Self`.
- Produces: `App::push_step_started(&mut self, step_id, name, args)`, `App::update_step_completed(&mut self, step_id, ok, output, truncated, full_len, error, duration_ms)`.
- Consumes: `StepCard` (Task 3), `streaming_agent_mut()` (existing), `markdown::render` (existing).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod step_app_tests {
    use super::*;

    fn app() -> App {
        // Reuse whatever the file's other tests use to build an App; if none,
        // construct via App::new(...) with a temp home. See existing tests in app.rs.
        App::test_fixture()
    }

    #[test]
    fn step_interleaves_between_text_segments() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.append_delta("reading the file", false);
        a.push_step_started("s1".into(), "read".into(), serde_json::json!({ "path": "a.rs" }));
        a.append_delta("done, here is the summary", false);

        // Expect: user, agent-seg-1 (frozen), tool card, agent-seg-2 (streaming)
        let roles: Vec<_> = a.messages.iter().map(|m| (m.role, m.step.is_some())).collect();
        assert_eq!(roles[0], (Role::User, false));
        assert_eq!(roles[1], (Role::Agent, false)); // text seg 1
        assert!(!a.messages[1].streaming);
        assert!(a.messages[2].step.is_some()); // the card
        assert_eq!(roles[3], (Role::Agent, false));
        assert!(a.messages[3].streaming); // text seg 2 still live
        assert_eq!(a.messages[3].text, "done, here is the summary");
    }

    #[test]
    fn completed_updates_the_matching_card() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.push_step_started("s1".into(), "read".into(), serde_json::json!({}));
        a.update_step_completed("s1", true, "412 lines".into(), false, 9, None, 8);
        let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
        assert_eq!(card.state, crate::cmd::agent::cli::step::StepState::Done);
        assert_eq!(card.output, "412 lines");
    }

    #[test]
    fn empty_leading_segment_is_dropped_not_frozen() {
        let mut a = app();
        a.begin_user_turn("hi"); // creates an empty streaming agent placeholder
        a.push_step_started("s1".into(), "read".into(), serde_json::json!({}));
        // The empty placeholder must not survive as a blank agent bubble.
        let agent_text_msgs = a
            .messages
            .iter()
            .filter(|m| m.role == Role::Agent && m.step.is_none())
            .count();
        assert_eq!(agent_text_msgs, 0);
    }
}
```

> If `app.rs` has no existing test constructor, add a minimal `#[cfg(test)] impl App { pub fn test_fixture() -> Self { /* mirror App::new with a tempdir home */ } }` — copy the field initializers from the real constructor (grep `impl App` / `fn new`). Keep it in the test module's parent so tests can call it.

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core step_app_tests 2>&1 | tail -20`
Expected: FAIL — `no field step` / `no method push_step_started`.

- [ ] **Step 3: Add the `step` field + `tool` ctor to `ChatMsg`**

In the struct (after `rendered`):

```rust
    /// When set, this message renders as a tool-call step card instead of by
    /// role. `None` for ordinary user/agent/system/shell messages.
    pub step: Option<super::step::StepCard>,
```

In `ChatMsg::new` and `ChatMsg::agent_rendered`, add `step: None,` to the struct literals. Add a ctor:

```rust
    /// A transcript entry that renders as a tool-call step card.
    fn tool(card: super::step::StepCard) -> Self {
        Self {
            role: Role::Agent,
            text: String::new(),
            thinking: String::new(),
            streaming: false,
            rendered: None,
            step: Some(card),
        }
    }
```

- [ ] **Step 4: Make `append_delta` create a streaming segment when none exists**

Replace `append_delta` body:

```rust
    pub fn append_delta(&mut self, text: &str, thinking: bool) {
        if self.streaming_agent_mut().is_none() {
            // The prior segment was frozen by a step card; start a new one.
            let mut m = ChatMsg::new(Role::Agent, "");
            m.streaming = true;
            self.messages.push(m);
        }
        if let Some(m) = self.streaming_agent_mut() {
            if thinking {
                m.thinking.push_str(text);
            } else {
                m.text.push_str(text);
            }
        }
        // NB: do not reset scroll_back here (see original note).
    }
```

- [ ] **Step 5: Add `push_step_started` + `update_step_completed`**

```rust
    /// Freeze the current streaming text segment (or drop it if empty) and push
    /// a new running tool-call card.
    pub fn push_step_started(
        &mut self,
        step_id: String,
        name: String,
        args: serde_json::Value,
    ) {
        if let Some(m) = self.streaming_agent_mut() {
            if m.text.is_empty() && m.thinking.is_empty() {
                // Empty placeholder (agent called a tool before any text) — drop it.
                if let Some(i) = self
                    .messages
                    .iter()
                    .rposition(|m| m.role == Role::Agent && m.streaming)
                {
                    self.messages.remove(i);
                }
            } else {
                m.streaming = false;
                m.rendered = Some(markdown::render(&m.text).lines);
            }
        }
        self.messages
            .push(ChatMsg::tool(super::step::StepCard::new(step_id, name, args)));
        self.scroll_back = 0;
    }

    /// Mark the matching card complete.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_completed(
        &mut self,
        step_id: &str,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
    ) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.complete(ok, output, truncated, full_len, error, duration_ms);
        }
    }
```

- [ ] **Step 6: Route the new `StreamMsg` variants in `handle_stream`** (`mod.rs`)

Add arms to the `match msg` (after `StreamMsg::Delta`):

```rust
        StreamMsg::StepStarted { step_id, name, args, .. } => {
            app.saw_step_this_turn = true;
            app.push_step_started(step_id, name, args);
        }
        StreamMsg::StepCompleted {
            step_id, ok, output, truncated, full_len, error, duration_ms, ..
        } => app.update_step_completed(
            &step_id, ok, output, truncated, full_len, error, duration_ms,
        ),
```

> `app.saw_step_this_turn` is added in Task 8. If implementing Task 4 before Task 8, temporarily drop that line and re-add it in Task 8.

- [ ] **Step 7: Run tests to verify they pass**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core step_app_tests 2>&1 | tail -20`
Expected: PASS (3 tests).

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): interleave tool-call step cards into the transcript"
```

---

### Task 5: Reasoning is kept, not erased (`app.rs` + `ui.rs`)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`finish_agent_turn` ~347–366 — remove `m.thinking.clear()`)
- Modify: `mur-core/src/cmd/agent/cli/ui.rs` (`push_message` Agent branch ~143–190 — render `thinking` for finished agent messages too)
- Test: `mur-core/src/cmd/agent/cli/app.rs` (inline test)

**Interfaces:**
- Consumes: existing `ChatMsg.thinking`, `streaming_agent_mut()`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod reasoning_kept_tests {
    use super::*;

    #[test]
    fn thinking_survives_turn_finish() {
        let mut a = App::test_fixture();
        a.begin_user_turn("hi");
        a.append_delta("let me think", true); // thinking delta
        a.append_delta("the answer", false);
        a.finish_agent_turn("the answer".into(), Some("t1".into()));
        let last = a.messages.last().unwrap();
        assert_eq!(last.role, Role::Agent);
        assert_eq!(last.thinking, "let me think"); // not cleared
        assert!(!last.streaming);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core reasoning_kept_tests 2>&1 | tail -20`
Expected: FAIL — `assert_eq!(last.thinking, "let me think")` fails (thinking is cleared).

- [ ] **Step 3: Remove the erase in `finish_agent_turn`**

Delete this line:

```rust
        m.thinking.clear();
```

- [ ] **Step 4: Render thinking for finished agent messages in `ui.rs`**

In `push_message`, the Agent branch currently only renders `m.thinking` inside `if m.streaming`. Hoist the thinking render so it runs for both states. Replace the Agent branch's opening:

```rust
        Role::Agent => {
            lines.push(Line::from(Span::styled(
                "● agent",
                Style::default().fg(theme.agent).add_modifier(Modifier::BOLD),
            )));
            // Reasoning stays visible after the turn finishes (D5).
            if !m.thinking.is_empty() {
                for l in m.thinking.lines() {
                    lines.push(Line::styled(
                        l.to_string(),
                        Style::default()
                            .fg(theme.thinking)
                            .add_modifier(Modifier::ITALIC | Modifier::DIM),
                    ));
                }
            }
            if m.streaming {
                // (spinner + streaming body — unchanged, but WITHOUT the
                // now-hoisted thinking block)
                let mut body: Vec<Line> =
                    m.text.lines().map(|l| Line::raw(l.to_string())).collect();
                let spin = SPINNER[spinner % SPINNER.len()];
                match body.last_mut() {
                    Some(last) => last.spans.push(Span::styled(
                        format!(" {spin}"),
                        Style::default().fg(theme.agent),
                    )),
                    None => body.push(Line::styled(
                        spin.to_string(),
                        Style::default().fg(theme.agent),
                    )),
                }
                lines.extend(body);
            } else if let Some(cached) = &m.rendered {
                lines.extend(cached.iter().cloned());
            } else {
                lines.extend(markdown::render(&m.text).lines);
            }
        }
```

- [ ] **Step 5: Run test + visual check**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core reasoning_kept_tests && cargo check -p mur-core`
Expected: PASS + clean build.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(cli): keep reasoning visible after the turn finishes"
```

---

### Task 6: Render the tool card (`ui.rs`)

**Files:**
- Create: `mur-core/src/cmd/agent/cli/render_card.rs` (card → `Vec<Line>`; keeps `ui.rs` from growing)
- Modify: `mur-core/src/cmd/agent/cli/ui.rs` (`push_message` ~100–105 — branch on `m.step` first)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (add `mod render_card;`)
- Test: `mur-core/src/cmd/agent/cli/render_card.rs` (inline test)

**Interfaces:**
- Produces: `render_card::card_lines(card: &StepCard, theme: &Theme) -> Vec<Line<'static>>`.
- Consumes: `StepCard` (Task 3), `theme::Theme`.

- [ ] **Step 1: Write the failing test** (create the file test-first)

```rust
#[cfg(test)]
mod tests {
    use super::card_lines;
    use crate::cmd::agent::cli::step::StepCard;
    use crate::cmd::agent::cli::theme;

    #[test]
    fn running_card_shows_glyph_name_and_no_result() {
        let c = StepCard::new("s1".into(), "read".into(), serde_json::json!({ "path": "a.rs" }));
        let lines = card_lines(&c, theme::resolve("dark"));
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("|");
        assert!(text.contains("read"));
        assert!(text.contains('◐'));
    }

    #[test]
    fn done_card_shows_output_and_duration() {
        let mut c = StepCard::new("s1".into(), "read".into(), serde_json::json!({}));
        c.complete(true, "412 lines".into(), false, 9, None, 8);
        let lines = card_lines(&c, theme::resolve("dark"));
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("412 lines"));
        assert!(text.contains("8ms"));
        assert!(text.contains('✔'));
    }
}
```

> Confirm the theme accessor name with `rg -n "pub fn resolve|fn resolve" mur-core/src/cmd/agent/cli/theme.rs`; the welcome/skin code resolves a `&'static Theme` by name. Use that exact function in the test.

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli::render_card 2>&1 | tail -20`
Expected: FAIL — module/function missing.

- [ ] **Step 3: Implement `render_card.rs`**

```rust
//! Render a `StepCard` to ratatui lines. Cards are expanded-by-default but
//! bounded: args capped at `ARGS_MAX_LINES`, output already capped upstream.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::step::{StepCard, StepState, ARGS_MAX_LINES};
use super::theme::Theme;

/// Lines after the header to show of a tool's output.
const OUTPUT_MAX_LINES: usize = 20;

pub fn card_lines(card: &StepCard, theme: &'static Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let accent = match card.state {
        StepState::Error => ratatui::style::Color::Red,
        _ => theme.agent,
    };

    // Header: glyph · name+arg-hint · state/duration
    let dur = card
        .duration_ms
        .map(|ms| format!(" · {ms}ms"))
        .unwrap_or_default();
    let header = format!("{} {} {}", card.glyph(), card.name, arg_hint(card));
    out.push(Line::from(vec![
        Span::styled(header, Style::default().fg(accent).add_modifier(Modifier::BOLD)),
        Span::styled(dur, Style::default().fg(theme.system)),
    ]));

    // Args (bounded)
    if !card.args.is_null() {
        let pretty = serde_json::to_string_pretty(&card.args).unwrap_or_default();
        for l in pretty.lines().take(ARGS_MAX_LINES) {
            out.push(Line::styled(
                format!("  {l}"),
                Style::default().fg(theme.system),
            ));
        }
        if pretty.lines().count() > ARGS_MAX_LINES {
            out.push(Line::styled(
                format!("  … +{} more", pretty.lines().count() - ARGS_MAX_LINES),
                Style::default().fg(theme.system).add_modifier(Modifier::DIM),
            ));
        }
    }

    // Result / error (bounded)
    if let Some(err) = &card.error {
        out.push(Line::styled(
            format!("  ✗ {err}"),
            Style::default().fg(ratatui::style::Color::Red),
        ));
    }
    if !card.output.is_empty() {
        for l in card.output.lines().take(OUTPUT_MAX_LINES) {
            out.push(Line::styled(
                format!("  {l}"),
                Style::default().fg(theme.text),
            ));
        }
        let shown = card.output.lines().count().min(OUTPUT_MAX_LINES);
        if card.output.lines().count() > OUTPUT_MAX_LINES || card.truncated {
            out.push(Line::styled(
                format!("  … {} more line(s) hidden", hidden_hint(card, shown)),
                Style::default().fg(theme.system).add_modifier(Modifier::DIM),
            ));
        }
    }
    out
}

fn arg_hint(card: &StepCard) -> String {
    card.args
        .as_object()
        .and_then(|m| m.values().find_map(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

fn hidden_hint(card: &StepCard, shown: usize) -> String {
    let total = card.output.lines().count();
    if card.truncated {
        "(output capped)".to_string()
    } else {
        (total - shown).to_string()
    }
}
```

- [ ] **Step 4: Register module + branch on `m.step` in `push_message`**

In `mod.rs`: `mod render_card;`

In `ui.rs::push_message`, add at the very top of the function (before the `match m.role`):

```rust
    if let Some(card) = &m.step {
        lines.extend(super::render_card::card_lines(card, theme));
        return;
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli::render_card 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/render_card.rs mur-core/src/cmd/agent/cli/ui.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): render tool-call step cards inline"
```

---

### Task 7: Footer math (`cli/footer.rs`)

**Files:**
- Create: `mur-core/src/cmd/agent/cli/footer.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`mod footer;`)
- Test: `mur-core/src/cmd/agent/cli/footer.rs` (inline test)

**Interfaces:**
- Produces: `UsageCounts { input, output }`, `Pricing { in_per_1k, out_per_1k, window }`.
- Produces: `parse_usage(&Value) -> UsageCounts`, `context_tokens(&Value) -> Option<u64>`, `turn_cost(&Pricing, &UsageCounts) -> Option<f64>`, `context_pct(used, window) -> u8`, `CtxColor::{Green,Yellow,Red}`, `ctx_color(u8) -> CtxColor`, `ctx_bar(pct, width) -> String`.
- Consumes: `serde_json::Value` (the `Task.usage` JSON).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_fields() {
        let u = parse_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 240 }));
        assert_eq!(u.input, 1000);
        assert_eq!(u.output, 240);
    }

    #[test]
    fn context_pct_is_input_over_window() {
        assert_eq!(context_pct(32_000, 100_000), 32);
        assert_eq!(context_pct(0, 100_000), 0);
        assert_eq!(context_pct(100, 0), 0); // no window → 0, never divide by zero
    }

    #[test]
    fn ctx_color_thresholds() {
        assert!(matches!(ctx_color(69), CtxColor::Green));
        assert!(matches!(ctx_color(70), CtxColor::Yellow));
        assert!(matches!(ctx_color(89), CtxColor::Yellow));
        assert!(matches!(ctx_color(90), CtxColor::Red));
    }

    #[test]
    fn cost_none_when_unpriced() {
        let u = UsageCounts { input: 1000, output: 1000 };
        let unpriced = Pricing { in_per_1k: None, out_per_1k: None, window: None };
        assert!(turn_cost(&unpriced, &u).is_none());
        let priced = Pricing { in_per_1k: Some(0.003), out_per_1k: Some(0.015), window: Some(200_000) };
        let c = turn_cost(&priced, &u).unwrap();
        assert!((c - 0.018).abs() < 1e-9);
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(ctx_bar(50, 6), "▓▓▓░░░");
        assert_eq!(ctx_bar(0, 6), "░░░░░░");
        assert_eq!(ctx_bar(100, 6), "▓▓▓▓▓▓");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli::footer 2>&1 | tail -20`
Expected: FAIL — module/types missing.

- [ ] **Step 3: Implement `footer.rs`**

```rust
//! Pure footer math: tokens, cost, and context-window fill from `Task.usage`
//! plus the agent's `models.yaml` pricing. No ratatui, no I/O — unit-tested.

use serde_json::Value;

/// Context bar thresholds (percent) and width.
pub const CTX_YELLOW_PCT: u8 = 70;
pub const CTX_RED_PCT: u8 = 90;
pub const CTX_BAR_WIDTH: usize = 6;

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageCounts {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Pricing {
    pub in_per_1k: Option<f64>,
    pub out_per_1k: Option<f64>,
    pub window: Option<u64>,
}

pub enum CtxColor {
    Green,
    Yellow,
    Red,
}

pub fn parse_usage(usage: &Value) -> UsageCounts {
    UsageCounts {
        input: usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output: usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
    }
}

/// Clean per-context fill emitted by the runtime (Task 1). `None` on older
/// runtimes — the caller falls back to hiding the bar.
pub fn context_tokens(usage: &Value) -> Option<u64> {
    usage.get("context_tokens").and_then(Value::as_u64)
}

pub fn turn_cost(p: &Pricing, u: &UsageCounts) -> Option<f64> {
    match (p.in_per_1k, p.out_per_1k) {
        (Some(i), Some(o)) => Some(u.input as f64 / 1000.0 * i + u.output as f64 / 1000.0 * o),
        _ => None,
    }
}

pub fn context_pct(used: u64, window: u64) -> u8 {
    if window == 0 {
        return 0;
    }
    ((used as f64 / window as f64) * 100.0).round().clamp(0.0, 100.0) as u8
}

pub fn ctx_color(pct: u8) -> CtxColor {
    if pct < CTX_YELLOW_PCT {
        CtxColor::Green
    } else if pct < CTX_RED_PCT {
        CtxColor::Yellow
    } else {
        CtxColor::Red
    }
}

pub fn ctx_bar(pct: u8, width: usize) -> String {
    let filled = (pct as usize * width) / 100;
    format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled))
}
```

- [ ] **Step 4: Register module + run test**

`mod.rs`: `mod footer;`

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli::footer 2>&1 | tail -20`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/footer.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): footer token/cost/context math module"
```

---

### Task 8: Footer state on `App` (timing, tokens, pricing)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`App` struct ~172–226; constructor; `begin_user_turn` ~314–327; `finish_agent_turn` ~347–366)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (startup: load pricing once and pass to `App`)
- Test: `mur-core/src/cmd/agent/cli/app.rs` (inline test)

**Interfaces:**
- Produces: `App.turn_started: Option<Instant>`, `App.session_in/session_out/turn_in/turn_out: u64`, `App.ctx_tokens: u64`, `App.pricing: footer::Pricing`, `App.saw_step_this_turn: bool`.
- Produces: `App::apply_usage(&mut self, usage: &Value)`.
- Produces (mod.rs): `fn load_pricing(home: &Path, agent: &str) -> footer::Pricing`.
- Consumes: `footer::{Pricing, parse_usage, context_tokens}`, `mur_common::model::ModelRegistry`, `load_profile_for_edit`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod footer_state_tests {
    use super::*;

    #[test]
    fn apply_usage_accumulates_session_and_sets_turn() {
        let mut a = App::test_fixture();
        a.apply_usage(&serde_json::json!({ "input_tokens": 100, "output_tokens": 20, "context_tokens": 100 }));
        a.apply_usage(&serde_json::json!({ "input_tokens": 50, "output_tokens": 10, "context_tokens": 150 }));
        assert_eq!(a.turn_in, 50);
        assert_eq!(a.turn_out, 10);
        assert_eq!(a.session_in, 150);
        assert_eq!(a.session_out, 30);
        assert_eq!(a.ctx_tokens, 150);
    }

    #[test]
    fn begin_turn_resets_turn_counters_and_starts_clock() {
        let mut a = App::test_fixture();
        a.apply_usage(&serde_json::json!({ "input_tokens": 100, "output_tokens": 20 }));
        a.begin_user_turn("hi");
        assert_eq!(a.turn_in, 0);
        assert_eq!(a.turn_out, 0);
        assert!(a.turn_started.is_some());
        assert!(!a.saw_step_this_turn);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core footer_state_tests 2>&1 | tail -20`
Expected: FAIL — fields/method missing.

- [ ] **Step 3: Add the fields to `App`** (in the struct)

```rust
    /// Wall-clock start of the in-flight turn (footer timer).
    pub turn_started: Option<std::time::Instant>,
    pub session_in: u64,
    pub session_out: u64,
    pub turn_in: u64,
    pub turn_out: u64,
    /// Input tokens of the latest request ≈ current context fill.
    pub ctx_tokens: u64,
    /// Per-1k pricing + window for the agent's model (`—`/no bar when absent).
    pub pricing: super::footer::Pricing,
    /// Whether any step event arrived this turn (proto-degrade hint, Task 10).
    pub saw_step_this_turn: bool,
```

In the `App` constructor (grep `fn new` in app.rs), initialize them:

```rust
            turn_started: None,
            session_in: 0,
            session_out: 0,
            turn_in: 0,
            turn_out: 0,
            ctx_tokens: 0,
            pricing: super::footer::Pricing::default(),
            saw_step_this_turn: false,
```

> The constructor must accept `pricing`. Add a `pricing: footer::Pricing` parameter to `App::new` (or set `app.pricing = ...` right after construction in `mod.rs`). The latter is the smaller diff — prefer it.

- [ ] **Step 4: Add `apply_usage` + wire `begin_user_turn` / `finish_agent_turn`**

```rust
    pub fn apply_usage(&mut self, usage: &serde_json::Value) {
        let u = super::footer::parse_usage(usage);
        self.turn_in = u.input;
        self.turn_out = u.output;
        self.session_in += u.input;
        self.session_out += u.output;
        if let Some(c) = super::footer::context_tokens(usage) {
            self.ctx_tokens = c;
        }
    }
```

In `begin_user_turn`, after `self.streaming = true;`:

```rust
        self.turn_started = Some(std::time::Instant::now());
        self.turn_in = 0;
        self.turn_out = 0;
        self.saw_step_this_turn = false;
```

In `finish_agent_turn`, the `Done` handler must call `apply_usage`. Easiest: in `mod.rs::handle_stream`, before `app.finish_agent_turn(...)`, pull usage from the task JSON:

```rust
        StreamMsg::Done { task, .. } => {
            if let Some(u) = task.get("usage") {
                app.apply_usage(u);
            }
            match stream::task_outcome(&task) {
                Ok((reply, task_id)) => app.finish_agent_turn(reply, task_id),
                Err(cause) => app.fail_turn(&cause),
            }
        }
```

Also clear the clock at finish (end of `finish_agent_turn`):

```rust
        self.turn_started = None;
```

- [ ] **Step 5: Add `load_pricing` in `mod.rs` and set it at startup**

```rust
/// Resolve the agent's per-1k pricing + context window from its profile and
/// `~/.mur/models.yaml`. Replicated here because mur-core must NOT depend on
/// mur-agent-runtime. Inline-model agents (no `model_ref`) have no pricing.
fn load_pricing(_home: &std::path::Path, agent: &str) -> super::cli::footer::Pricing {
    use mur_common::model::ModelRegistry;
    let pricing = super::cli::footer::Pricing::default();
    let Ok((_, profile)) = crate::cmd::agent::load_profile_for_edit(agent) else {
        return pricing;
    };
    let Some(model_ref) = profile.model_ref.as_deref() else {
        return pricing; // inline model: no registry pricing
    };
    let Ok(path) = ModelRegistry::default_path() else { return pricing };
    let Ok(reg) = ModelRegistry::load_from(&path) else { return pricing };
    let Some(entry) = reg.models.get(model_ref) else { return pricing };
    let (input, output) = entry.effective_costs();
    super::cli::footer::Pricing {
        in_per_1k: input,
        out_per_1k: output,
        window: entry.context_window,
    }
}
```

> Adjust the module path (`super::cli::footer` vs `footer`) to match where this fn lives relative to the `footer` module. If `mod.rs` IS the cli module root, use `footer::Pricing`. Confirm the import path for `load_profile_for_edit` with `rg -n "fn load_profile_for_edit" mur-core/src`.

After constructing `App` in the cli entry (`run_tui` / wherever `App::new` is called), set:

```rust
    app.pricing = load_pricing(&app.home, &app.agent);
```

- [ ] **Step 6: Run test + build**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core footer_state_tests && cargo check -p mur-core`
Expected: PASS + clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): track per-turn/session tokens + pricing + turn clock on App"
```

---

### Task 9: Render the parity footer (`ui.rs`)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/ui.rs` (`render_status` ~192–270)
- Test: manual (render assertions on a status are awkward; the math is already unit-tested in Task 7). Add a small pure helper test for the assembled left-segment string.

**Interfaces:**
- Consumes: `App.{streaming, hitl, turn_started, turn_in, turn_out, session_in, session_out, ctx_tokens, pricing}`, `footer::{turn_cost, context_pct, ctx_color, ctx_bar, CTX_BAR_WIDTH, UsageCounts}`.

- [ ] **Step 1: Write the failing test** (pure formatter)

Add to `ui.rs`:

```rust
#[cfg(test)]
mod footer_fmt_tests {
    use super::footer_segments;
    use crate::cmd::agent::cli::footer::Pricing;

    #[test]
    fn shows_tokens_and_dash_cost_when_unpriced() {
        let s = footer_segments(
            1240, 0, 1240, 0, 0,
            &Pricing::default(),
        );
        assert!(s.contains("1,240 tok") || s.contains("1240 tok"));
        assert!(s.contains("—")); // no price → em dash
    }

    #[test]
    fn shows_cost_and_ctx_when_priced() {
        let p = Pricing { in_per_1k: Some(0.003), out_per_1k: Some(0.015), window: Some(100_000) };
        let s = footer_segments(1000, 1000, 1000, 1000, 32_000, &p);
        assert!(s.contains("$0.018"));
        assert!(s.contains("32%"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core footer_fmt_tests 2>&1 | tail -20`
Expected: FAIL — `footer_segments` missing.

- [ ] **Step 3: Add `footer_segments` + rewrite the status bar**

Add the pure formatter to `ui.rs`:

```rust
/// Build the footer's observability text (state glyph is added by the caller).
/// `turn_in/turn_out` = this turn; `sess_in/sess_out` = running; `ctx` = fill.
fn footer_segments(
    turn_in: u64,
    turn_out: u64,
    sess_in: u64,
    sess_out: u64,
    ctx: u64,
    pricing: &super::footer::Pricing,
) -> String {
    use super::footer::{context_pct, ctx_bar, turn_cost, UsageCounts, CTX_BAR_WIDTH};
    let turn_tok = turn_in + turn_out;
    let sess_tok = sess_in + sess_out;
    let cost = turn_cost(pricing, &UsageCounts { input: turn_in, output: turn_out })
        .map(|c| format!("${c:.3} est"))
        .unwrap_or_else(|| "—".to_string());
    let ctx_part = match pricing.window {
        Some(w) if w > 0 => {
            let pct = context_pct(ctx, w);
            format!(" · ctx {} {}%", ctx_bar(pct, CTX_BAR_WIDTH), pct)
        }
        _ => String::new(),
    };
    format!("{turn_tok}/{sess_tok} tok · {cost}{ctx_part}")
}
```

In `render_status`, keep the existing `(msg, color)` state logic, but append the footer segments + a wall timer when streaming. After the `spans.push(Span::styled(msg, ...))` line, add:

```rust
    // Glass Box observability: tokens · cost · ctx · timer.
    let obs = footer_segments(
        app.turn_in, app.turn_out, app.session_in, app.session_out, app.ctx_tokens, &app.pricing,
    );
    spans.push(Span::raw("  ·  "));
    spans.push(Span::styled(obs, Style::default().fg(theme.system)));
    if let Some(t0) = app.turn_started {
        let secs = t0.elapsed().as_secs();
        spans.push(Span::styled(
            format!(" · {}m{:02}s · esc=stop", secs / 60, secs % 60),
            Style::default().fg(theme.system),
        ));
    }
```

> The status row is `Constraint::Length(1)` — a single line. If the combined text overflows narrow terminals, ratatui clips it (acceptable for P1). Widening to two rows is a P2 polish; do NOT change the layout here.

- [ ] **Step 4: Run test + visual build**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core footer_fmt_tests && cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings`
Expected: PASS + clean.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(cli): parity footer — tokens, cost, context bar, turn timer"
```

---

### Task 10: Proto-degrade hint for old runtimes

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`handle_stream` `Done` arm)
- Modify: `mur-core/src/cmd/agent/cli/stream.rs` (add `task_used_tools(task: &Value) -> bool` helper)
- Test: `mur-core/src/cmd/agent/cli/stream.rs` (inline test)

**Interfaces:**
- Produces: `stream::task_used_tools(&Value) -> bool` (did the final task contain any tool_use messages?).
- Consumes: `App.saw_step_this_turn` (Task 8), `App.push_system` (existing).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod degrade_tests {
    use super::task_used_tools;

    #[test]
    fn detects_tool_use_in_messages() {
        let task = serde_json::json!({
            "messages": [
                { "role": "assistant", "parts": [ { "kind": "tool_use", "name": "read" } ] }
            ]
        });
        assert!(task_used_tools(&task));
    }

    #[test]
    fn false_when_no_tools() {
        let task = serde_json::json!({
            "messages": [ { "role": "assistant", "parts": [ { "kind": "text", "text": "hi" } ] } ]
        });
        assert!(!task_used_tools(&task));
    }
}
```

> Confirm the message/part shape with `rg -n "tool_use|ToolUse|kind" mur-common/src/a2a.rs` and adjust the predicate to match the real serialized form (it may be `type`/`tool_call` rather than `kind`/`tool_use`). The test must reflect the actual JSON.

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core degrade_tests 2>&1 | tail -20`
Expected: FAIL — `task_used_tools` missing.

- [ ] **Step 3: Implement `task_used_tools`** (in `stream.rs`)

```rust
/// True if the final task JSON contains any tool-use message part — used to
/// detect an old runtime that ran tools but never streamed `step/*` events.
pub fn task_used_tools(task: &Value) -> bool {
    task.get("messages")
        .and_then(Value::as_array)
        .map(|msgs| {
            msgs.iter().any(|m| {
                m.get("parts")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts.iter().any(|p| {
                            p.get("kind").and_then(Value::as_str) == Some("tool_use")
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}
```

- [ ] **Step 4: Emit the one-time hint in `handle_stream`'s `Done` arm**

After `app.apply_usage(...)` and before/around `finish_agent_turn`, add:

```rust
            if !app.saw_step_this_turn && stream::task_used_tools(&task) && !app.step_hint_shown {
                app.step_hint_shown = true;
                app.push_system(
                    "↻ this agent ran tools but didn't stream step detail — restart it (mur agent restart <name>) for the Glass Box step view",
                );
            }
```

Add `pub step_hint_shown: bool,` to `App` (init `false`) so the hint shows at most once per session.

- [ ] **Step 5: Run test + build**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core degrade_tests && cargo check -p mur-core`
Expected: PASS + clean.

- [ ] **Step 6: Final P1 verification + commit**

Run:
```bash
ORT_STRATEGY=download cargo nextest run -p mur-core cmd::agent::cli
cargo nextest run -p mur-agent-runtime step_tests
cargo clippy -p mur-core -p mur-agent-runtime -- -D warnings
cargo fmt --check
```
Expected: all green.

```bash
git add mur-core/src/cmd/agent/cli/stream.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): one-time hint when an old runtime omits step events"
```

---

## Manual verification (after all tasks)

1. Build + install: `./build.sh --install` (or `cargo build --release -p mur-core`).
2. Start an agent with a `model_ref` set (so pricing shows): `mur agent cli <name>`.
3. Ask it to read a file and run a shell command. Confirm:
   - Each tool appears as a `◐ name …` card that flips to `✔`/`✗` with a duration.
   - Args + (bounded) output render under the card.
   - Reasoning stays on screen after the reply (doesn't vanish).
   - Footer shows `turn/session tok`, `$cost est` (or `—` for an inline-model agent), `ctx ▓▓░░ N%` (priced agent), and a live `0m08s · esc=stop` timer while streaming.
4. Point the cli at an **old** runtime binary (pre-Task-1). Confirm: no cards, no panic, and the one-time "restart for step view" hint appears after a tool-using turn.

## Self-Review (completed)

- **Spec coverage:** D0 (T1–T2), D1 reasoning-unchanged-protocol (T2 keeps `thinking` deltas; T5 keeps them on screen), D2 truncate-inline (T1 `cap_step_output`, full-expand deferred per spec), D3 proto-degrade (T10), D6 footer input-only/`est`/`—` (T7–T9), card render (T6). D4 (inline HITL), D5 toggle, D7 compaction hook, diff viewer, mid-turn steer, budget, plain-mode → **P2/P3, not this plan** (per spec phasing). ✔
- **Placeholder scan:** none — every step has runnable code/commands. The three "confirm with `rg`" notes are verification anchors against real code, not logic placeholders. ✔
- **Type consistency:** `StepEvent`/`StepCard`/`StreamMsg::Step*`/`footer::*`/`apply_usage`/`saw_step_this_turn`/`step_hint_shown` names match across T1–T10. ✔
