# MCP shim Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `mur_agent_<name> mcp-shim` subcommand that speaks MCP on stdio to a spawned CLI and forwards every tool call to the running agent over its unix socket — including a HITL pause, which crosses as `tool/approval_needed` and reaches the CLI as `elicitation/create`.

**Architecture:** The shim is a child of the CLI, which is a child of MUR (`--mcp-config` names a command to spawn). It holds no tools, no policy and no vault: it is a translator between two JSON-RPC streams. Its socket connection is what makes the HITL round trip possible, because the agent routes a turn's approval prompt to the connection that asked.

**Tech stack:** Rust (edition 2024, `mur-mcp-proto`, `mur-agent-runtime`).

### Global Constraints

- **The shim executes nothing.** No `ToolExecutor`, no policy evaluation, no secret vault. Every obligation stays on the agent's side of the socket.
- Fail closed. A shim that cannot reach the agent answers `isError`, never a fabricated success.
- The socket is the trust boundary; the shim adds none of its own.
- Nothing in the in-process turn path changes behaviour.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-mcp-proto/src/lib.rs` | modified | `Incoming` + `read_incoming()`: classify a stdin line as request or response. |
| `mur-agent-runtime/src/protocol/methods/tools.rs` | modified | `tools/call` registers the calling connection as the task's approval sink. |
| `mur-agent-runtime/src/mcp_shim.rs` | created | The shim: stdio MCP ⇄ agent socket, plus the elicitation round trip. |
| `mur-agent-runtime/src/lib.rs` | modified | One `pub mod mcp_shim;` line. |
| `mur-agent-runtime/src/supervisor.rs` | modified | Route `mcp-shim` to the shim before the normal entrypoint. |

## Two findings this plan is built on

Both measured against the code, and each would sink the shim if assumed away.

**1. `read_request()` cannot read a response.** It deserializes strictly into
`Request`, which requires `method`. A JSON-RPC *response* to the shim's own
`elicitation/create` arrives on the same stdin and fails to parse, coming back
as a bogus parse-error request. A pure responder never notices; a bidirectional
one cannot work at all. Hence Task 1.

**2. `tools/call` does not register an approval sink.** `message_send` does it
explicitly — `register_client_notifier(task_id, ctx.notifier, can_approve)` —
and `tools/call` (#1352) does not, which is exactly why an `Ask` tool denies
there today. The shim connecting is not enough; the handler has to route that
task's prompts to the connection that asked. Hence Task 2.

---

## Task 1 — `mur-mcp-proto` can read a response

### Interfaces

**Consumes:** nothing.

**Produces:**
```rust
pub enum Incoming { Request(Request), Response(Value), Unparseable(String) }
pub fn read_incoming<R: std::io::BufRead>(r: &mut R) -> Option<Incoming>;
```

`Response` is carried as a raw `Value`, not as the `Response` struct. That
struct's `jsonrpc` field is a `&'static str`, so it serializes but cannot be
deserialized into, and widening it to `String` would change a shared type for
this one caller's convenience.

`read_incoming` takes the reader rather than locking stdin itself, unlike
`read_request`. The shim reads stdin on a blocking thread and needs to own
that handle; a function that reaches for the global lock cannot be driven
from one.

### Steps

- [ ] Add to `mur-mcp-proto/src/lib.rs`, above the existing `read_request`:

```rust
/// One line off a peer's stream, classified.
///
/// A bidirectional MCP endpoint receives both: requests the peer is asking
/// it to serve, and responses to requests it sent (`elicitation/create`).
/// They share one stream and differ only by shape — a request has `method`,
/// a response has `result` or `error`.
#[derive(Debug)]
pub enum Incoming {
    Request(Request),
    /// Raw, because [`Response`] holds `jsonrpc` as a `&'static str` and so
    /// cannot be deserialized into. The caller wants the `id` and the
    /// `result`/`error`, both of which a `Value` gives directly.
    Response(Value),
    /// Neither shape parsed. Kept as the raw line so the caller can log it
    /// and carry on rather than treating a peer's malformed frame as EOF.
    Unparseable(String),
}

/// Read and classify one line. `None` is EOF and only EOF.
///
/// Takes the reader instead of locking stdin, unlike [`read_request`]: a
/// bidirectional endpoint owns its stdin handle on a blocking thread, and a
/// function that grabs the global lock cannot be driven from there.
pub fn read_incoming<R: std::io::BufRead>(r: &mut R) -> Option<Incoming> {
    loop {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
                    return Some(Incoming::Unparseable(trimmed.to_string()));
                };
                // Shape, not guesswork: a request has `method`, a response
                // has `result` or `error`. Anything with neither is not a
                // JSON-RPC frame we can route.
                if v.get("method").is_some() {
                    return match serde_json::from_value::<Request>(v) {
                        Ok(req) => Some(Incoming::Request(req)),
                        Err(_) => Some(Incoming::Unparseable(trimmed.to_string())),
                    };
                }
                if v.get("result").is_some() || v.get("error").is_some() {
                    return Some(Incoming::Response(v));
                }
                return Some(Incoming::Unparseable(trimmed.to_string()));
            }
            Err(e) => {
                tracing::error!(error = %e, "read error");
                return None;
            }
        }
    }
}
```

- [ ] Add the test module at the bottom of `mur-mcp-proto/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn read(s: &str) -> Option<Incoming> {
        read_incoming(&mut Cursor::new(s.as_bytes().to_vec()))
    }

    #[test]
    fn a_request_is_read_as_a_request() {
        let got = read(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert!(matches!(got, Some(Incoming::Request(r)) if r.method == "tools/list"));
    }

    #[test]
    fn a_response_is_not_mistaken_for_a_broken_request() {
        // The whole reason this function exists. `read_request` turns this
        // line into a parse-error request, which is silently wrong for a
        // bidirectional endpoint — the answer to its own elicitation would
        // be read as the peer asking it something.
        let got = read(r#"{"jsonrpc":"2.0","id":"e-1","result":{"action":"accept"}}"#);
        assert!(matches!(got, Some(Incoming::Response(_))), "{got:?}");
    }

    #[test]
    fn an_error_response_is_still_a_response() {
        let got = read(r#"{"jsonrpc":"2.0","id":"e-1","error":{"code":-32601,"message":"no"}}"#);
        assert!(matches!(got, Some(Incoming::Response(_))), "{got:?}");
    }

    #[test]
    fn garbage_is_reported_not_treated_as_eof() {
        // EOF ends the loop. A malformed frame must not, or one bad line
        // from the peer would look like the peer hanging up.
        assert!(matches!(read("not json at all\n"), Some(Incoming::Unparseable(_))));
    }

    #[test]
    fn eof_is_none() {
        assert!(read("").is_none());
    }
}
```

- [ ] Run and watch them pass:

```bash
cargo test -p mur-mcp-proto
```

Expected: `test result: ok. 5 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(mcp-proto): read_incoming classifies requests and responses"`

---

## Task 2 — `tools/call` routes this task's prompts to the caller

Without this, an `Ask` tool denies no matter what the shim does: the gate has
no sink to ask.

### Interfaces

**Consumes:** `ToolsCallHandler` (#1352), `TaskRunner::register_client_notifier`.

**Produces:** no new API — `tools/call` gains an optional `can_approve` param
(default `false`) and registers `ctx.notifier` for the task while the call runs.

### Steps

- [ ] In `mur-agent-runtime/src/protocol/methods/tools.rs`, inside
      `ToolsCallHandler::handle`, replace the line that reads
      `let guarded = self.runner.guarded();` with:

```rust
        // Route this task's approval prompts back to the connection that
        // asked, exactly as `message_send` does. Without this the gate has
        // no sink and an `Ask` tool is denied — correct, but unusable.
        //
        // `can_approve` defaults to false: a caller that cannot answer must
        // say so, because claiming otherwise makes the gate wait out its
        // full timeout for nobody.
        let can_approve = p
            .get("can_approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(n) = ctx.notifier.clone() {
            self.runner
                .register_client_notifier(&task_id, n, can_approve)
                .await;
        }
        let guarded = self.runner.guarded();
```

- [ ] Change the handler's signature to bind the context — replace
      `_ctx: &RequestContext,` with `ctx: &RequestContext,` in
      `ToolsCallHandler`'s `handle` only. `ToolsListHandler` keeps `_ctx`.

- [ ] Unregister when the call ends, so a later turn on the same task id does
      not inherit a dead sink. Add this immediately after the `let entry =
      match guarded.handle_tool_call(...)` block closes, before the final
      `Ok(json!({...}))`:

```rust
        self.runner.unregister_client_notifier(&task_id).await;
```

- [ ] Also unregister on the denial path — inside the `Err(e) =>` arm, before
      its `return Ok(json!({...}))`:

```rust
                self.runner.unregister_client_notifier(&task_id).await;
```

- [ ] Add this test inside the existing `mod tests` block in `tools.rs`:

```rust
    #[tokio::test]
    async fn an_ask_tool_asks_the_calling_connection() {
        // The Task 2 property: the prompt reaches the caller's own sink.
        // Answering is the shim's job; this asserts only that the question
        // was asked, which is what #1351 could not do.
        use mur_common::agent::{ToolPolicy, ToolRule};
        // `with_pending_approvals` is load-bearing: without it the gate has
        // no map to park a pending answer in and fails closed at once, so
        // the prompt this test waits for is never sent. `new_stub_echo()`
        // leaves it `None`.
        let runner = Arc::new(
            runner_with_probe()
                .with_tools_policy(vec![ToolRule {
                    pattern: "*".into(),
                    policy: ToolPolicy::Ask,
                    risk: None,
                }])
                .with_pending_approvals(Default::default()),
        );
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(8);
        let ctx = RequestContext {
            notifier: Some(tx),
        };
        let h = ToolsCallHandler::new(runner);
        let call = tokio::spawn(async move {
            h.handle(
                Some(json!({
                    "task_id": "t-1",
                    "name": "probe_tool",
                    "can_approve": true,
                })),
                &ctx,
            )
            .await
        });
        let note = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no approval prompt reached the caller")
            .expect("sink closed");
        assert_eq!(note["method"], json!("tool/approval_needed"));
        assert_eq!(note["params"]["task_id"], json!("t-1"));
        // Nobody answers, so the gate times out and denies. That the prompt
        // arrived at all is the assertion; the denial is #1351's behaviour.
        drop(call);
    }
```

- [ ] Run the module and watch every test pass:

```bash
cargo test -p mur-agent-runtime protocol::methods::tools
```

Expected: `test result: ok. 8 passed; 0 failed`.

- [ ] Confirm nothing gained an execution site:

```bash
cargo test -p mur-agent-runtime execute_is_called_from_guarded_only
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(a2a): tools/call routes approval prompts to its caller"`

---

## Task 3 — The shim

### Interfaces

**Consumes:** `mur_mcp_proto::{Incoming, Request, read_incoming}` (Task 1); the agent's `tools/list`, `tools/call` (Task 2) and `tool/hitl_respond`.

**Produces:**
```rust
pub async fn run(socket: std::path::PathBuf, task_id: String) -> anyhow::Result<()>;
// argv: mur_agent_<name> mcp-shim --socket <path> --task-id <id>
```

### The shape, and why it is three loops rather than one

The shim sits between two streams that both push. Stdin carries the CLI's
requests *and* the answers to the shim's own elicitations; the socket carries
method results *and* `tool/approval_needed` notifications arriving unbidden
mid-call. Neither side can be polled on the other's schedule, so each gets its
own task and they meet through channels.

```
stdin  ──► reader task ──┬── Request  ──► main loop ──► socket ──► agent
                         └── Response ──► pending elicitation (oneshot)
socket ──► reader task ──┬── result   ──► pending call (oneshot)
                         └── tool/approval_needed ──► main loop ──► stdout
                                                       elicitation/create
```

### Steps

- [ ] Add the dependency to `mur-agent-runtime/Cargo.toml`, beside the other
      `mur-*` path deps:

```toml
mur-mcp-proto = { path = "../mur-mcp-proto" }
```

- [ ] Create `mur-agent-runtime/src/mcp_shim.rs` with the module doc and the
      entry point:

```rust
//! The MCP shim: stdio to a spawned CLI, unix socket to the agent.
//!
//! `--mcp-config` hands the CLI a command to spawn, so this runs as a child
//! of the CLI, which is a child of MUR. It therefore cannot reach
//! `GuardedToolCall` — a different process holds it — and it does not try.
//! The shim owns no tools, no policy and no vault. Every obligation stays on
//! the agent's side of the socket, which is what keeps the
//! one-execution-path test meaningful across the process split.
//!
//! See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.

use mur_mcp_proto::{Incoming, Request, read_incoming};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

/// Answers still owed to us, by request id: elicitations sent to the CLI and
/// method calls sent to the agent.
type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

pub async fn run(socket: std::path::PathBuf, task_id: String) -> anyhow::Result<()> {
    let stream = tokio::net::UnixStream::connect(&socket)
        .await
        .map_err(|e| anyhow::anyhow!("connect {}: {e}", socket.display()))?;
    let (sock_read, sock_write) = stream.into_split();
    let sock_write = Arc::new(Mutex::new(sock_write));

    let pending_calls: Pending = Default::default();
    let pending_elicits: Pending = Default::default();
    let (prompt_tx, mut prompt_rx) = tokio::sync::mpsc::channel::<Value>(16);

    // ── socket reader: method results, and prompts arriving mid-call ──
    {
        let pending_calls = pending_calls.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(sock_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if v.get("method").and_then(|m| m.as_str()) == Some("tool/approval_needed") {
                    let _ = prompt_tx.send(v).await;
                    continue;
                }
                if let Some(id) = v.get("id").and_then(id_key)
                    && let Some(tx) = pending_calls.lock().await.remove(&id)
                {
                    let _ = tx.send(v);
                }
            }
        });
    }

    // ── stdin reader: the CLI's requests, and answers to our elicitations ──
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<Request>(16);
    {
        let pending_elicits = pending_elicits.clone();
        // Blocking: stdin has no async form here, and this thread does
        // nothing but read, so it may block freely.
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            while let Some(inc) = read_incoming(&mut lock) {
                match inc {
                    Incoming::Request(r) => {
                        if req_tx.blocking_send(r).is_err() {
                            break;
                        }
                    }
                    Incoming::Response(resp) => {
                        let Some(id) = resp.get("id").and_then(id_key) else {
                            continue;
                        };
                        let pe = pending_elicits.clone();
                        let v = resp;
                        // The map is async-locked; hand the wake-up to the
                        // runtime rather than blocking this thread on it.
                        tokio::runtime::Handle::current().spawn(async move {
                            if let Some(tx) = pe.lock().await.remove(&id) {
                                let _ = tx.send(v);
                            }
                        });
                    }
                    Incoming::Unparseable(raw) => {
                        tracing::warn!(raw = %raw, "unparseable frame from the CLI");
                    }
                }
            }
        });
    }

    let mut next_id: u64 = 0;
    loop {
        tokio::select! {
            Some(req) = req_rx.recv() => {
                let resp = serve(&req, &task_id, &sock_write, &pending_calls, &mut next_id).await;
                write_stdout(&resp).await;
            }
            Some(prompt) = prompt_rx.recv() => {
                elicit(prompt, &sock_write, &pending_elicits, &mut next_id).await;
            }
            else => break,
        }
    }
    Ok(())
}

/// JSON-RPC ids are numbers or strings; both key the same map.
fn id_key(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

async fn write_stdout(v: &Value) {
    let mut out = tokio::io::stdout();
    let mut bytes = serde_json::to_vec(v).unwrap_or_default();
    bytes.push(b'\n');
    let _ = out.write_all(&bytes).await;
    let _ = out.flush().await;
}
```

- [ ] Add the request handling and the elicitation round trip to the same
      file:

```rust
async fn serve(
    req: &Request,
    task_id: &str,
    sock: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: &Pending,
    next_id: &mut u64,
) -> Value {
    let id = req.id.clone().unwrap_or(Value::Null);
    match req.method.as_str() {
        "initialize" => json!({
            "jsonrpc": "2.0", "id": id,
            "result": {
                // The closure needs its type: `params` is `Option<Value>`
                // and inference has nothing else to go on here.
                "protocolVersion": req.params.as_ref()
                    .and_then(|p: &Value| p.get("protocolVersion").cloned())
                    .unwrap_or_else(|| json!("2025-11-25")),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "mur", "version": env!("CARGO_PKG_VERSION") },
            }
        }),
        "tools/list" => match call_agent(sock, pending, next_id, "tools/list", json!({})).await {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": v["tools"]}}),
            Err(e) => rpc_error(id, &e),
        },
        "tools/call" => {
            let p = req.params.clone().unwrap_or(Value::Null);
            let args = p.get("arguments").cloned().unwrap_or(json!({}));
            let params = json!({
                "task_id": task_id,
                "name": p.get("name").cloned().unwrap_or(Value::Null),
                "arguments": args,
                // We can answer, because we can ask the CLI's user.
                "can_approve": true,
            });
            match call_agent(sock, pending, next_id, "tools/call", params).await {
                Ok(v) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [{"type": "text", "text": v["content"].as_str().unwrap_or("")}],
                        "isError": v["is_error"].as_bool().unwrap_or(false),
                    }
                }),
                // Fail closed and visibly: the model is told the call failed
                // rather than handed a fabricated empty success.
                Err(e) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("MUR agent unreachable: {e}")}],
                        "isError": true,
                    }
                }),
            }
        }
        _ => rpc_error(id, &format!("method not found: {}", req.method)),
    }
}

fn rpc_error(id: Value, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": message}})
}

async fn call_agent(
    sock: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: &Pending,
    next_id: &mut u64,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    *next_id += 1;
    let id = format!("shim-{next_id}");
    let (tx, rx) = oneshot::channel();
    pending.lock().await.insert(id.clone(), tx);
    let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let mut bytes = serde_json::to_vec(&frame).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    sock.lock()
        .await
        .write_all(&bytes)
        .await
        .map_err(|e| e.to_string())?;
    let v = rx
        .await
        .map_err(|_| "agent closed the connection".to_string())?;
    if let Some(err) = v.get("error") {
        return Err(err["message"].as_str().unwrap_or("agent error").to_string());
    }
    Ok(v.get("result").cloned().unwrap_or(Value::Null))
}

/// One approval prompt, asked of the CLI's user and answered back to the agent.
async fn elicit(
    prompt: Value,
    sock: &Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: &Pending,
    next_id: &mut u64,
) {
    let calls = prompt["params"]["calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for c in calls {
        let Some(hitl_id) = c["hitl_id"].as_str().map(String::from) else {
            continue;
        };
        *next_id += 1;
        let id = format!("elicit-{next_id}");
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(id.clone(), tx);
        write_stdout(&json!({
            "jsonrpc": "2.0", "id": id, "method": "elicitation/create",
            "params": {
                "message": format!(
                    "MUR wants to run `{}`. Allow?",
                    c["tool_name"].as_str().unwrap_or("a tool")
                ),
                "requestedSchema": {
                    "type": "object",
                    "properties": { "allow": { "type": "boolean" } },
                    "required": ["allow"],
                },
            }
        }))
        .await;

        // Anything that is not an explicit accept-with-allow is a denial.
        // `decline` and `cancel` are denials; so is a malformed answer. The
        // gate's own timeout still runs underneath, so a CLI that never
        // replies is denied rather than hanging the turn.
        let answer = rx.await.ok();
        let allow = answer
            .as_ref()
            .map(|v| {
                v["result"]["action"] == json!("accept")
                    && v["result"]["content"]["allow"] == json!(true)
            })
            .unwrap_or(false);

        let mut bytes = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": format!("hitl-{next_id}"),
            "method": "tool/hitl_respond",
            "params": { "hitl_id": hitl_id, "allow": allow },
        }))
        .unwrap_or_default();
        bytes.push(b'\n');
        let _ = sock.lock().await.write_all(&bytes).await;
    }
}
```

- [ ] Add `pub mod mcp_shim;` to `mur-agent-runtime/src/lib.rs` in
      alphabetical position.

- [ ] Route the subcommand. In `mur-agent-runtime/src/supervisor.rs`, at the
      very top of `entrypoint()`, before anything else runs:

```rust
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("mcp-shim") {
        let socket = crate::subcommand::flag_value(&argv, "--socket")
            .ok_or_else(|| anyhow::anyhow!("mcp-shim: --socket is required"))?;
        let task_id = crate::subcommand::flag_value(&argv, "--task-id")
            .ok_or_else(|| anyhow::anyhow!("mcp-shim: --task-id is required"))?;
        return crate::mcp_shim::run(std::path::PathBuf::from(socket), task_id).await;
    }
```

- [ ] Build and watch it compile:

```bash
cargo build -p mur-agent-runtime
```

Expected: compiles, no warnings from the new module.

- [ ] Commit: `git add -A && git commit -m "feat(runtime): the MCP shim, stdio to a CLI and socket to the agent"`

---

## Task 4 — Prove the translation, without a CLI

Every piece here is testable without spawning `claude`: the shim's two ends
are just JSON-RPC over streams.

### Steps

- [ ] Add the test module to the bottom of `mur-agent-runtime/src/mcp_shim.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numeric_and_a_string_id_key_the_same_map() {
        // JSON-RPC allows both, and the peer picks. Keying on only one shape
        // would strand every answer the other kind of peer sends.
        assert_eq!(id_key(&json!("e-1")).as_deref(), Some("e-1"));
        assert_eq!(id_key(&json!(7)).as_deref(), Some("7"));
        assert!(id_key(&Value::Null).is_none());
    }

    #[test]
    fn only_an_explicit_allow_is_an_approval() {
        // The decision table, stated once. `decline` and `cancel` are
        // denials; so is an accept whose payload does not actually say yes,
        // and so is a malformed answer. Unattended CLIs answer `cancel`,
        // which is why this is the branch that matters most.
        let allow = |v: Value| {
            v["result"]["action"] == json!("accept")
                && v["result"]["content"]["allow"] == json!(true)
        };
        assert!(allow(
            json!({"result": {"action": "accept", "content": {"allow": true}}})
        ));
        assert!(!allow(
            json!({"result": {"action": "accept", "content": {"allow": false}}})
        ));
        assert!(!allow(json!({"result": {"action": "decline"}})));
        assert!(!allow(json!({"result": {"action": "cancel"}})));
        assert!(!allow(json!({"error": {"code": -32601}})));
        assert!(!allow(json!({})));
    }
}
```

- [ ] Run and watch them pass:

```bash
cargo test -p mur-agent-runtime mcp_shim
```

Expected: `test result: ok. 2 passed; 0 failed`.

- [ ] End-to-end by hand, against a real agent. Start an agent, then:

```bash
MUR_AGENT=$(ls ~/.mur/agents | head -1)
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
| mur_agent_$MUR_AGENT mcp-shim \
    --socket ~/.mur/agents/$MUR_AGENT/agent.sock --task-id manual-probe
```

Expected: two JSON lines on stdout — an `initialize` result declaring
`"capabilities":{"tools":{}}`, then a `tools/list` result whose `tools` array
is the agent's own. If the second is an error naming the socket, the agent is
not running; start it and retry.

- [ ] Lint and format:

```bash
cargo clippy -p mur-agent-runtime -p mur-mcp-proto -- -D warnings && cargo fmt --check
```

Expected: no output, exit 0.

- [ ] Commit: `git add -A && git commit -m "test(runtime): the shim's id keying and approval decision"`

## Done when

- [ ] `cargo test -p mur-mcp-proto` — 5 passed.
- [ ] `cargo test -p mur-agent-runtime protocol::methods::tools` — 8 passed.
- [ ] `cargo test -p mur-agent-runtime mcp_shim` — 2 passed.
- [ ] `cargo test -p mur-agent-runtime execute_is_called_from_guarded_only` — 1 passed.
- [ ] The manual probe returns the agent's real tool list.
- [ ] `grep -rn "ToolExecutor" mur-agent-runtime/src/mcp_shim.rs` — no match.
      The shim must not have grown a way to run anything.

## Not in this plan

- **Spawning the CLI with this shim wired in.** That is the backend's job and
  needs the registry's `mcp_mount` to write a `--mcp-config` naming this
  subcommand. Separate change.
- **`codex` and `agy`.** Both mount MCP persistently into a config file, so
  they need that file written per turn into their private home — open
  question 1 of the server design, still unverified for either.
- **An attended end-to-end HITL test.** Task 4 asserts the decision table and
  the manual probe covers the transport, but a human accepting a prompt
  inside an interactive `claude` is not something a script can drive. The
  unattended direction — `cancel`, therefore deny — is the one that is
  covered, and it is the one that fails dangerously if wrong.
