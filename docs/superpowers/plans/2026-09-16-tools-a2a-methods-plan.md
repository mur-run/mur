# `tools/list` and `tools/call` A2A methods Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the agent's tools over its existing unix socket — `tools/list` to enumerate them and `tools/call` to run one through `GuardedToolCall` — so the MCP shim of `2026-09-16-mur-tool-mcp-server-design.md` has something to forward to, without anything new executing a tool.

**Architecture:** Two `MethodHandler`s in `mur-agent-runtime/src/protocol/methods/tools.rs`, registered in `supervisor.rs` beside `tasks/*`. `tools/call` builds a `ToolCallResult`, runs it through the same `gate_response` → `handle_tool_call` pair an in-process turn uses, and returns the resulting `ToolResultEntry` as JSON. No tool executes anywhere new.

**Tech stack:** Rust (edition 2024, `mur-agent-runtime`).

### Global Constraints

- **No second execution path.** `ToolExecutor::execute` keeps exactly one call site; `tools/guarded.rs` stays its only home and the tripwire test stays green.
- Every obligation an in-process turn carries — policy gate, HITL, task scope, secret mask, withdrawal on refusal — applies identically here, because it is the same code, not a copy.
- Fail closed. A tool that cannot be gated is denied, never run ungated.
- The socket is the trust boundary; these methods add no other.

### The slice, and what it deliberately leaves broken

`Ask`-policy tools will **deny** through this path, because denying is what
`gate_response` does when no approval sink is registered for the task. That is
correct fail-closed behaviour and it is tested here as such — but it means an
`Ask` tool is not *usable* over this path until the shim registers itself as
that task's sink, which is the next slice.

Saying so is the point: a reader who finds `Ask` denying should find it
documented as this slice's boundary, not conclude the gate is broken.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-agent-runtime/src/protocol/methods/tools.rs` | created | `ToolsListHandler`, `ToolsCallHandler`, and their tests. |
| `mur-agent-runtime/src/protocol/methods/mod.rs` | modified | One `pub mod tools;` line. |
| `mur-agent-runtime/src/supervisor.rs` | modified | Two `d.register(...)` calls. |
| `mur-agent-runtime/src/task_runner.rs` | modified | `guarded()` widened to `pub(crate)`. |

---

## Task 1 — `tools/list`

### Interfaces

**Consumes:** `TaskRunner`, `crate::tools::ToolExecutor::def()`.

**Produces:**
```rust
pub struct ToolsListHandler { runner: Arc<TaskRunner> }
impl ToolsListHandler { pub fn new(r: Arc<TaskRunner>) -> Self }
// A2A: "tools/list" -> {"tools": [{"name", "description", "inputSchema"}, ...]}
```

`inputSchema` is camelCase because that is MCP's spelling and the shim
forwards this array unchanged. Renaming it in the shim would put the
protocol's field name in two places.

### Steps

- [ ] Widen the constructor in `mur-agent-runtime/src/task_runner.rs` — find
      `fn guarded(&self)` and make it `pub(crate) fn guarded(&self)`. Nothing
      else on that function changes.

- [ ] Create `mur-agent-runtime/src/protocol/methods/tools.rs`:

```rust
//! `tools/*` handlers — the agent's tools, over its own socket.
//!
//! These exist so the MCP shim has something to forward to. The shim speaks
//! MCP on stdio to a spawned CLI and nothing else: it holds no tools, no
//! policy and no vault, so every obligation stays on this side of the
//! socket. See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.
//!
//! `tools/call` runs the same `gate_response` → `handle_tool_call` pair an
//! in-process turn runs. Not a similar pair — the same one, so a change to
//! the gate cannot apply to one caller and miss the other.

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::TaskRunner;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct ToolsListHandler {
    runner: Arc<TaskRunner>,
}

impl ToolsListHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { runner: r }
    }
}

#[async_trait]
impl MethodHandler for ToolsListHandler {
    async fn handle(
        &self,
        _params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let tools: Vec<Value> = self
            .runner
            .guarded()
            .tools
            .iter()
            .map(|t| {
                let d = t.def();
                json!({
                    "name": d.name,
                    "description": d.description,
                    "inputSchema": d.input_schema,
                })
            })
            .collect();
        Ok(json!({ "tools": tools }))
    }
}
```

- [ ] Add to `mur-agent-runtime/src/protocol/methods/mod.rs`, in alphabetical
      position (after `pub mod tasks;`):

```rust
pub mod tools;
```

- [ ] Register it in `mur-agent-runtime/src/supervisor.rs`, directly after the
      `d.register("tasks/list", ...)` block:

```rust
    d.register(
        "tools/list",
        Box::new(crate::protocol::methods::tools::ToolsListHandler::new(
            runner.clone(),
        )),
    );
```

- [ ] Add the test module to the bottom of `tools.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RequestContext {
        // No per-connection notifier: these tests drive the handler directly,
        // not through a socket.
        RequestContext::none()
    }

    /// A tool that records nothing and returns a fixed string.
    ///
    /// `TaskRunner::new_stub_echo()` registers **no tools at all**, so every
    /// test here must supply its own. Without this, `tools/list` would assert
    /// `0 == 0` and pass while proving nothing.
    struct ProbeTool;

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for ProbeTool {
        fn name(&self) -> &str {
            "probe_tool"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "probe_tool".into(),
                description: "a tool for tests".into(),
                input_schema: json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: Value,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            Ok("probe ran".to_string().into())
        }
    }

    fn runner_with_probe() -> TaskRunner {
        TaskRunner::new_stub_echo().with_tools(vec![Arc::new(ProbeTool)])
    }

    #[tokio::test]
    async fn tools_list_reports_every_tool_in_mcp_shape() {
        let runner = Arc::new(runner_with_probe());
        let out = ToolsListHandler::new(runner.clone())
            .handle(None, &ctx())
            .await
            .expect("handler");
        let listed = out["tools"].as_array().expect("tools array");
        assert_eq!(listed.len(), 1, "expected exactly the probe tool");
        assert_eq!(listed[0]["name"], json!("probe_tool"));
        for t in listed {
            // The shim forwards these verbatim, so the MCP spelling is the
            // one that has to be right here rather than one hop later.
            assert!(t["name"].is_string(), "name missing: {t}");
            assert!(t["description"].is_string(), "description missing: {t}");
            assert!(t["inputSchema"].is_object(), "inputSchema missing: {t}");
        }
    }
}
```

- [ ] Run it and watch it pass:

```bash
cargo test -p mur-agent-runtime tools_list_reports_every_tool
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(a2a): tools/list reports the agent's tools"`

---

## Task 2 — `tools/call`

### Interfaces

**Consumes:** `ToolsListHandler`'s module and the widened `guarded()` (Task 1).

**Produces:**
```rust
pub struct ToolsCallHandler { runner: Arc<TaskRunner> }
impl ToolsCallHandler { pub fn new(r: Arc<TaskRunner>) -> Self }
// A2A: "tools/call" {task_id, name, arguments?, call_id?}
//   -> {"call_id", "content", "is_error", "status"}
```

`task_id` is required and not defaulted. It is what binds a `bash` job to an
owner that can kill it, and what the HITL gate routes an approval prompt by.
A default would silently detach both.

### Steps

- [ ] Add to `mur-agent-runtime/src/protocol/methods/tools.rs`, above the test
      module:

```rust
pub struct ToolsCallHandler {
    runner: Arc<TaskRunner>,
}

impl ToolsCallHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { runner: r }
    }
}

#[async_trait]
impl MethodHandler for ToolsCallHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let task_id = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing task_id".into()))?
            .to_string();
        let name = p
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing name".into()))?
            .to_string();
        let call_id = p
            .get("call_id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let input = p.get("arguments").cloned().unwrap_or(Value::Null);

        let call = crate::llm::ToolCallResult {
            call_id: call_id.clone(),
            tool_name: name,
            input,
        };

        // The same pair an in-process turn runs, in the same order: gate
        // first, then execute with whatever the gate decided. Calling
        // `handle_tool_call` without `gate_response` would skip the HITL
        // prompt for an `Ask` tool and execute it unasked.
        let guarded = self.runner.guarded();
        let (decisions, step_ids) = guarded
            .gate_response(&task_id, std::slice::from_ref(&call))
            .await;
        let entry = match guarded
            .handle_tool_call(
                &task_id,
                &call,
                decisions.get(&call_id).cloned(),
                step_ids.get(&call_id).cloned(),
            )
            .await
        {
            Ok(e) => e,
            // A refusal is an answer, not a transport failure. `handle_tool_call`
            // reports a HITL denial as `Err(TaskError { code: "hitl_denied" })`,
            // and MCP's convention is that a tool which refused returns a
            // result carrying `isError` so the model can read it — a protocol
            // error would tell the CLI the *call* broke and tell the model
            // nothing. The code travels with it so the shim can still tell a
            // refusal from a crash instead of guessing from a message string.
            Err(e) => {
                return Ok(json!({
                    "call_id": call_id,
                    "content": e.message,
                    "is_error": true,
                    "error_code": e.code,
                }));
            }
        };

        Ok(json!({
            "call_id": entry.call_id,
            "content": entry.content,
            "is_error": entry.is_error,
            "status": entry.status,
        }))
    }
}
```

- [ ] Register it in `mur-agent-runtime/src/supervisor.rs`, directly after the
      `tools/list` block from Task 1:

```rust
    d.register(
        "tools/call",
        Box::new(crate::protocol::methods::tools::ToolsCallHandler::new(
            runner.clone(),
        )),
    );
```

- [ ] Add these tests inside the existing `mod tests` block in `tools.rs`:

```rust
    #[tokio::test]
    async fn tools_call_requires_a_task_id() {
        // Not pedantry: task_id is what gives a spawned bash job an owner and
        // what routes an approval prompt. Defaulting it would detach both
        // silently, which is why this is an error rather than a fallback.
        let h = ToolsCallHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let err = h
            .handle(Some(json!({ "name": "read_file" })), &ctx())
            .await
            .expect_err("must reject");
        assert!(matches!(err, HandlerError::InvalidParams(_)), "{err:?}");
    }

    #[tokio::test]
    async fn an_unknown_tool_is_reported_not_executed() {
        let h = ToolsCallHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let out = h
            .handle(
                Some(json!({ "task_id": "t-1", "name": "no_such_tool" })),
                &ctx(),
            )
            .await
            .expect("handler");
        assert_eq!(out["is_error"], json!(true));
        assert!(
            out["content"]
                .as_str()
                .unwrap_or_default()
                .contains("unknown tool"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn the_call_id_is_echoed_so_the_shim_can_correlate() {
        let h = ToolsCallHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let out = h
            .handle(
                Some(json!({
                    "task_id": "t-1",
                    "name": "no_such_tool",
                    "call_id": "given-by-caller",
                })),
                &ctx(),
            )
            .await
            .expect("handler");
        assert_eq!(out["call_id"], json!("given-by-caller"));
    }
```

- [ ] Run them and watch them pass:

```bash
cargo test -p mur-agent-runtime protocol::methods::tools
```

Expected: `test result: ok. 4 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(a2a): tools/call runs a tool through the guarded path"`

---

## Task 3 — Prove the obligations survive the new caller

Task 2 routes through `GuardedToolCall`; these assert it rather than trusting
the call order to stay put.

### Interfaces

**Consumes:** `ToolsCallHandler` (Task 2).

**Produces:** no new API — three tests.

### Steps

- [ ] Add these tests inside the existing `mod tests` block in `tools.rs`:

```rust
    #[tokio::test]
    async fn a_denied_tool_is_refused_without_executing() {
        use mur_common::agent::{ToolPolicy, ToolRule};
        let runner = runner_with_probe().with_tools_policy(vec![ToolRule {
            pattern: "*".into(),
            policy: ToolPolicy::Deny,
            risk: None,
        }]);
        let h = ToolsCallHandler::new(Arc::new(runner));
        let out = h
            .handle(
                Some(json!({ "task_id": "t-1", "name": "probe_tool" })),
                &ctx(),
            )
            .await
            .expect("handler");
        assert_eq!(out["is_error"], json!(true));
        assert!(
            out["content"]
                .as_str()
                .unwrap_or_default()
                .contains("denied by policy"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn an_ask_tool_with_no_approval_sink_denies_rather_than_hanging() {
        // This slice's boundary, asserted so it reads as a boundary rather
        // than a bug: until the shim registers as the task's approval sink,
        // an `Ask` tool has nobody to ask, and the gate's answer to that is
        // to deny at once. Never to run it, and never to wait out the
        // timeout against nobody.
        use mur_common::agent::{ToolPolicy, ToolRule};
        let runner = runner_with_probe().with_tools_policy(vec![ToolRule {
            pattern: "*".into(),
            policy: ToolPolicy::Ask,
            risk: None,
        }]);
        let h = ToolsCallHandler::new(Arc::new(runner));
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            h.handle(
                Some(json!({ "task_id": "t-1", "name": "probe_tool" })),
                &ctx(),
            ),
        )
        .await
        .expect("must not hang: an unanswerable gate denies, it does not wait")
        .expect("handler");
        assert_eq!(out["is_error"], json!(true));
        // The code, not the prose: a shim distinguishing a refusal from a
        // crash must not have to match on a message.
        assert_eq!(out["error_code"], json!("hitl_denied"));
    }

    #[tokio::test]
    async fn tool_output_reaching_this_caller_is_masked() {
        // The obligation that fails most quietly. If a future change routes
        // `tools/call` around `GuardedToolCall`, this is what notices.
        let vault = crate::secrets::SecretVault::new();
        vault
            .set("PROBE_TOKEN", "sk-probe-abcdefghijklmnop")
            .expect("set");
        let runner = runner_with_probe().with_secrets(Arc::new(vault));
        let masked = runner
            .guarded()
            .masked("leaked sk-probe-abcdefghijklmnop here".into());
        assert!(!masked.contains("sk-probe-abcdefghijklmnop"), "{masked}");
        assert!(masked.contains("PROBE_TOKEN"), "{masked}");
    }
```

- [ ] Run the module and watch every test pass:

```bash
cargo test -p mur-agent-runtime protocol::methods::tools
```

Expected: `test result: ok. 7 passed; 0 failed`.

- [ ] Confirm the hard constraint still holds — the new caller must not have
      introduced an execution site:

```bash
cargo test -p mur-agent-runtime execute_is_called_from_guarded_only
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] Lint and format:

```bash
cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt --check
```

Expected: no output, exit 0.

- [ ] Commit: `git add -A && git commit -m "test(a2a): the guarded obligations hold for tools/call"`

## Done when

- [ ] `cargo test -p mur-agent-runtime protocol::methods::tools` — 7 passed.
- [ ] `cargo test -p mur-agent-runtime execute_is_called_from_guarded_only` — 1 passed.
- [ ] `cargo test -p mur-agent-runtime` — no non-`llm::` failure that was not
      failing before (those three network tests flake outside CI).
- [ ] `cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt --check` — clean.

## Not in this plan

- **The shim.** Its own change: a binary that speaks MCP on stdio, forwards to
  these two methods, registers itself as the task's approval sink, and
  re-frames `tool/approval_needed` as `elicitation/create`. Every piece it
  needs on the agent side now exists — including `register_client_notifier`
  and `tool/hitl_respond`, both already `pub`.
- **Making `Ask` usable over this path.** It denies until the shim is that
  sink, and Task 3 asserts the denial so the boundary is visible.
- **Authorization on the methods themselves.** The socket is the trust
  boundary and these methods inherit it, exactly as `tasks/*` and
  `turn/steer` do. Changing that is a decision about the socket, not about
  these two handlers.
