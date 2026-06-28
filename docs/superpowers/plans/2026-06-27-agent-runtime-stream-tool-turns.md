# Agent Runtime — Stream Reasoning + Text During Tool Turns — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the agent's tool-using (agentic) loop stream the model's reasoning + text deltas live, so the cli shows what the model is thinking *during* a tool turn instead of a silent wall of cards.

**Architecture:** The loop already receives a `sink` (`tokio::sync::mpsc::Sender<StreamDelta>`) but ignores it (`_sink`) because it calls non-streaming `client.generate`. The blocker: Anthropic's `generate_stream` is incomplete — its SSE parser handles only text/thinking deltas and returns `tool_calls: vec![]` + `stop_reason: EndTurn`, so the tool loop can't use it (it would think every turn ended). Fix Anthropic's `generate_stream` to reconstruct `tool_use` blocks (from `content_block_start` + `input_json_delta`) and read the real `stop_reason`, then swap the loop's call from `generate` to `generate_stream`. Non-Anthropic providers already work via the default `generate_stream` (which delegates to `generate`, preserving tool_calls). **No cli change needed** — P1's segment-interleaving + reply-drop fix were built for exactly this stream shape.

**Tech Stack:** Rust (edition 2024), reqwest SSE (`resp.chunk()`), serde_json, `async_trait` (`LlmClient`).

## Global Constraints

- **Independent of the Glass Box cli PRs** — this is `mur-agent-runtime` only. Branch from `main` (fetch first; release CI advances main). Mergeable on its own; the streaming is *seen* best with the P1 cli (#517), but the runtime change is safe with the current cli too (it already handles `message/delta`).
- **Rust edition 2024**; no hardcoded values (named `const`).
- **Tests:** runtime tests run with the toolchain cargo if rustup is broken: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"`, then plain `cargo test -p mur-agent-runtime` (the `cargo-nextest` binary is absent).
- **Lint gate:** `cargo clippy -p mur-agent-runtime -- -D warnings` + `cargo fmt`.
- **Behavior parity is the bar:** `generate_stream` must return a `LlmResponse` equivalent to `generate` for the same response (same `text`, `tool_calls` with `{call_id, tool_name, input}`, and `stop_reason`), while additionally streaming deltas. The non-stream `parse_response_body` (anthropic.rs:279-324) is the reference: tool_use block `id→call_id`, `name→tool_name`, `input→input`; `stop_reason` `"tool_use"→ToolUse`, `"max_tokens"→MaxTokens`, else `EndTurn`.

---

### Task 1: Complete Anthropic `generate_stream` — stream tool_use + stop_reason

**Files:**
- Modify: `mur-agent-runtime/src/llm/anthropic.rs` (the SSE event handling inside `generate_stream` ~415-560; extract a testable `apply_sse_event` + `StreamAccum`; relax the empty-response guard)
- Test: `mur-agent-runtime/src/llm/anthropic.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: `StreamAccum` (text/input_tokens/output_tokens/tool_calls/stop_reason/cur_tool) + `fn apply_sse_event(acc: &mut StreamAccum, v: &serde_json::Value) -> Option<StreamDelta>`.
- Consumes: `StreamDelta`, `ToolCallResult`, `StopReason`, `LlmResponse` (llm/mod.rs).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn apply_sse_event_streams_text_and_reasoning() {
    let mut acc = StreamAccum::default();
    let d = apply_sse_event(&mut acc, &serde_json::json!({
        "type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}
    }));
    assert_eq!(d.as_ref().map(|x| (x.text.as_str(), x.thinking)), Some(("hello", false)));
    assert_eq!(acc.text, "hello");

    let d = apply_sse_event(&mut acc, &serde_json::json!({
        "type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}
    }));
    assert_eq!(d.as_ref().map(|x| (x.text.as_str(), x.thinking)), Some(("hmm", true)));
    // reasoning is streamed but NOT accumulated into the answer text
    assert_eq!(acc.text, "hello");
}

#[test]
fn apply_sse_event_reconstructs_tool_use_and_stop_reason() {
    let mut acc = StreamAccum::default();
    apply_sse_event(&mut acc, &serde_json::json!({
        "type":"content_block_start","index":0,
        "content_block":{"type":"tool_use","id":"call_1","name":"bash","input":{}}
    }));
    apply_sse_event(&mut acc, &serde_json::json!({
        "type":"content_block_delta","index":0,
        "delta":{"type":"input_json_delta","partial_json":"{\"command\":"}
    }));
    apply_sse_event(&mut acc, &serde_json::json!({
        "type":"content_block_delta","index":0,
        "delta":{"type":"input_json_delta","partial_json":"\"echo hi\"}"}
    }));
    apply_sse_event(&mut acc, &serde_json::json!({"type":"content_block_stop","index":0}));
    apply_sse_event(&mut acc, &serde_json::json!({
        "type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}
    }));

    assert_eq!(acc.tool_calls.len(), 1);
    assert_eq!(acc.tool_calls[0].call_id, "call_1");
    assert_eq!(acc.tool_calls[0].tool_name, "bash");
    assert_eq!(acc.tool_calls[0].input, serde_json::json!({"command":"echo hi"}));
    assert_eq!(acc.stop_reason, StopReason::ToolUse);
    assert_eq!(acc.output_tokens, 7);
}

#[test]
fn apply_sse_event_no_arg_tool_defaults_to_empty_object() {
    let mut acc = StreamAccum::default();
    apply_sse_event(&mut acc, &serde_json::json!({
        "type":"content_block_start","content_block":{"type":"tool_use","id":"c","name":"now","input":{}}
    }));
    apply_sse_event(&mut acc, &serde_json::json!({"type":"content_block_stop"}));
    assert_eq!(acc.tool_calls[0].input, serde_json::json!({}));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime apply_sse_event 2>&1 | tail -20`
Expected: FAIL — `StreamAccum` / `apply_sse_event` not found.

- [ ] **Step 3: Add `StreamAccum` + `apply_sse_event`** (above `generate_stream` in anthropic.rs)

```rust
use super::{StopReason, StreamDelta, ToolCallResult};

/// Accumulator for an Anthropic SSE response while it streams.
struct StreamAccum {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    tool_calls: Vec<ToolCallResult>,
    stop_reason: StopReason,
    /// The in-progress tool_use block: (id, name, partial-JSON args buffer).
    cur_tool: Option<(String, String, String)>,
}

impl Default for StreamAccum {
    fn default() -> Self {
        Self {
            text: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            cur_tool: None,
        }
    }
}

/// Apply one parsed SSE `data:` event to `acc`. Returns a `StreamDelta` to
/// forward to the sink iff this event carried answer text or reasoning.
/// Mirrors the non-stream `parse_response_body` for tool_use + stop_reason.
fn apply_sse_event(acc: &mut StreamAccum, v: &serde_json::Value) -> Option<StreamDelta> {
    match v["type"].as_str() {
        Some("content_block_start") => {
            let cb = &v["content_block"];
            if cb["type"].as_str() == Some("tool_use") {
                acc.cur_tool = Some((
                    cb["id"].as_str().unwrap_or("").to_string(),
                    cb["name"].as_str().unwrap_or("").to_string(),
                    String::new(),
                ));
            } else {
                acc.cur_tool = None;
            }
            None
        }
        Some("content_block_delta") => {
            let d = &v["delta"];
            match d["type"].as_str() {
                Some("text_delta") => {
                    let t = d["text"].as_str().unwrap_or("");
                    if t.is_empty() {
                        return None;
                    }
                    acc.text.push_str(t);
                    Some(StreamDelta { text: t.to_string(), thinking: false })
                }
                Some("thinking_delta") => {
                    let t = d["thinking"].as_str().unwrap_or("");
                    if t.is_empty() {
                        return None;
                    }
                    Some(StreamDelta { text: t.to_string(), thinking: true })
                }
                Some("input_json_delta") => {
                    if let (Some((_, _, buf)), Some(pj)) =
                        (acc.cur_tool.as_mut(), d["partial_json"].as_str())
                    {
                        buf.push_str(pj);
                    }
                    None
                }
                _ => None,
            }
        }
        Some("content_block_stop") => {
            if let Some((id, name, buf)) = acc.cur_tool.take() {
                let input = if buf.trim().is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_str(&buf).unwrap_or_else(|_| serde_json::json!({}))
                };
                acc.tool_calls.push(ToolCallResult { call_id: id, tool_name: name, input });
            }
            None
        }
        Some("message_start") => {
            acc.input_tokens = v["message"]["usage"]["input_tokens"]
                .as_u64()
                .unwrap_or(acc.input_tokens);
            None
        }
        Some("message_delta") => {
            if let Some(sr) = v["delta"]["stop_reason"].as_str() {
                acc.stop_reason = match sr {
                    "tool_use" => StopReason::ToolUse,
                    "max_tokens" => StopReason::MaxTokens,
                    _ => StopReason::EndTurn,
                };
            }
            acc.output_tokens = v["usage"]["output_tokens"]
                .as_u64()
                .unwrap_or(acc.output_tokens);
            None
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Rewrite `generate_stream`'s SSE loop to use the accumulator**

Replace the loop body's locals + `match v["type"]` block + the final return. The fn HEAD (request setup through the `while let Some(chunk)` line) stays unchanged. Inside the chunk loop, after parsing `v`, replace the inline `match v["type"].as_str() { … }` with:

```rust
            if let Some(delta) = apply_sse_event(&mut acc, &v) {
                let _ = sink.send(delta).await;
            }
```

Initialize `let mut acc = StreamAccum::default();` in place of the old `text`/`input_tokens`/`output_tokens` locals. Replace the final block (the old `if text.is_empty() { return Err(...) }` + the `Ok(LlmResponse { … tool_calls: vec![], stop_reason: EndTurn })`) with:

```rust
    // A tool-only response legitimately has empty text — only error if BOTH
    // the answer text and the tool calls are empty.
    if acc.text.is_empty() && acc.tool_calls.is_empty() {
        return Err(LlmError::InvalidResponse("empty streamed response".into()));
    }
    Ok(LlmResponse {
        text: acc.text,
        input_tokens: acc.input_tokens,
        output_tokens: acc.output_tokens,
        model: self.model.clone(),
        tool_calls: acc.tool_calls,
        stop_reason: acc.stop_reason,
    })
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-agent-runtime apply_sse_event 2>&1 | tail -20`
Expected: PASS (3 tests). Also run the existing anthropic tests to confirm no regression: `cargo test -p mur-agent-runtime --lib llm::anthropic 2>&1 | tail -20`.

- [ ] **Step 6: Lint + commit**

Run: `cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`
Expected: clean.

```bash
git add mur-agent-runtime/src/llm/anthropic.rs
git commit -m "feat(runtime): Anthropic generate_stream reconstructs tool_use + stop_reason (was text-only)"
```

---

### Task 2: Stream the agentic loop

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (`run_agentic_loop` ~1269-1331: rename `_sink`→`sink`; swap the per-iteration `generate` → `generate_stream`)
- Test: behavior is covered by Task 1 (the streamed `LlmResponse` is now complete) + manual run; add no fake test for the loop.

**Interfaces:**
- Consumes: `LlmClient::generate_stream` (now complete for Anthropic, default-delegating for others); the `sink: Option<Sender<StreamDelta>>` the loop already receives from `run_sync_streaming`.

- [ ] **Step 1: Use the sink**

Rename the parameter `_sink` to `sink` in `run_agentic_loop`'s signature (~line 1276):
```rust
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
```

Replace the per-iteration LLM call (currently lines 1328-1331):
```rust
        let resp = client
            .generate(req)
            .await
            .map_err(|e| task_error("llm_error", format!("{e}"), true))?;
```
with the streaming-when-available form (mirrors `run_llm`'s `match sink`):
```rust
        let resp = match &sink {
            Some(s) => client.generate_stream(req, s.clone()).await,
            None => client.generate(req).await,
        }
        .map_err(|e| task_error("llm_error", format!("{e}"), true))?;
```

> `generate_stream` takes the `Sender` by value, so clone the sink each iteration (`s.clone()`). The loop's history accumulation, token counting, tool extraction, and exit condition (`if resp.tool_calls.is_empty() || resp.stop_reason == StopReason::EndTurn`) are unchanged — `resp` is the same complete `LlmResponse` either way.

- [ ] **Step 2: Build + lint** (the change must not leave `sink` partially-moved across loop iterations — cloning handles that)

Run: `cargo check -p mur-agent-runtime && cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`
Expected: clean (no "use of moved value" — `s.clone()` per iteration; `None` arm unchanged).

- [ ] **Step 3: Full runtime test suite (no regressions)**

Run: `cargo test -p mur-agent-runtime 2>&1 | grep -E "test result|error\[|FAILED" | tail`
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): stream reasoning + text during the agentic (tool) loop"
```

---

## Manual verification (after both tasks)

1. Build the runtime: `cargo build --release -p mur-agent-runtime`. (For a visible effect, also build the P1 cli: `cargo build --release -p mur-core`.)
2. Restart a tool-using agent onto the new runtime (repoint `~/.local/bin/mur_agent_<name>` → the new `target/release/mur-agent-runtime`, then `mur agent restart <name>`).
3. `./target/release/mur agent cli <agent>` (P1 build); ask it something that needs a tool ("read Cargo.toml and summarize"). Confirm:
   - the model's **reasoning + text now stream live** *during* the turn (before/between the tool cards), not just the final answer;
   - tool cards still appear and the agent still **runs the tools** (the loop didn't break — `tool_calls`/`stop_reason` survived streaming);
   - the final answer renders correctly.
4. Sanity: a no-tool question still streams (unchanged `run_llm` path).

## Out of scope

- `graceful_exit` (the loop's final-summary turn) still uses non-streaming `generate` — rare, low value; leave it.
- OpenAI/Ollama true streaming overrides — they use the default `generate_stream` (delegates to `generate`: correct tool_calls, whole-text-once). True per-token streaming for them is a separate follow-up.

## Self-Review (completed)

- **Spec coverage:** Anthropic streaming tool_use + stop_reason (T1), loop swap (T2). ✔
- **Placeholder scan:** none — every step has runnable code/commands. ✔
- **Type consistency:** `apply_sse_event(&mut StreamAccum, &Value) -> Option<StreamDelta>` (T1) drives the loop; `generate_stream(req, sink)` consumed in T2 with `s.clone()` per iteration. `StopReason`/`ToolCallResult`/`LlmResponse` fields match llm/mod.rs. ✔
