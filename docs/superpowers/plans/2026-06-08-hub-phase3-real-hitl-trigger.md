# Hub Phase 3 — Real HITL Trigger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire a tool-use loop into `TaskRunner` so actual LLM tool calls pause for Hub approval before execution — closing the Phase 2 HITL story.

**Architecture:** Add `RichMessage` type hierarchy to `llm/mod.rs` (replacing `LlmMessage` in `LlmRequest`), update Anthropic and OpenAI adapters to serialize tool schemas and parse `tool_use`/`tool_calls` blocks, add `ToolExecutor` trait + `BashTool`, extend `TaskRunner` with `run_agentic_loop` that gates each call through the existing `pending_approvals` mechanism, extend `HitlCard` with deny-with-reason UI.

**Tech Stack:** Rust/Tokio (`mur-agent-runtime`, `mur-common`), TypeScript/React (`mur-hub-gui/ui`), Tauri 2 commands, `reqwest`/`serde_json`.

**Disk note:** Firecuda4tb is nearly full (~80 MB free). `cargo build` may fail with ENOSPC. If it does, run `cargo clean -p <target-crate>` to free space before retrying.

**Test command:** `cargo test -p <crate>` (nextest not installed in PATH).

---

## File Map

| File | Action |
|---|---|
| `mur-agent-runtime/src/llm/mod.rs` | Modify — add `ToolDef`, `ToolCallResult`, `StopReason`, `RichMessage`, `ToolResultEntry`; change `LlmRequest.messages`; extend `LlmResponse` |
| `mur-agent-runtime/src/llm/anthropic.rs` | Modify — `RichMessage→wire`, tool schema, `tool_use` parse |
| `mur-agent-runtime/src/llm/openai.rs` | Modify — `RichMessage→wire`, function-calling, `tool_calls` parse |
| `mur-agent-runtime/src/tools/mod.rs` | Create — `ToolExecutor` trait |
| `mur-agent-runtime/src/tools/bash.rs` | Create — `BashTool` |
| `mur-agent-runtime/src/lib.rs` | Modify — `pub mod tools;` |
| `mur-agent-runtime/src/hitl.rs` | Create — `HitlDecision` type |
| `mur-agent-runtime/src/supervisor.rs` | Modify — change `pending_approvals` type, `HitlRespondHandler` extracts reason |
| `mur-agent-runtime/src/supervisor_runner.rs` | Modify — thread `pending_approvals`/`notifier` into `build_runner` |
| `mur-agent-runtime/src/task_runner.rs` | Modify — new fields, `run_agentic_loop`, `handle_tool_call` |
| `mur-common/src/agent.rs` | Modify — `HitlConfig.max_iterations` |
| `mur-hub-gui/src-tauri/src/hitl.rs` | Modify — `reason: Option<String>` param |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Modify — deny-with-reason UI |
| `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` | Modify — 4 new hitl keys |

---

### Task 1: Core LLM types in `llm/mod.rs` + `task_runner.rs` compat

**Files:**
- Modify: `mur-agent-runtime/src/llm/mod.rs`
- Modify: `mur-agent-runtime/src/task_runner.rs`

- [ ] **Step 1: Write failing tests in `llm/mod.rs`**

Add at the bottom of `mur-agent-runtime/src/llm/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_message_text_roundtrip() {
        let m = RichMessage::Text { role: "user".into(), content: "hello".into() };
        match &m {
            RichMessage::Text { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content, "hello");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn llm_request_tools_defaults_empty() {
        let req = LlmRequest {
            messages: vec![RichMessage::Text { role: "user".into(), content: "hi".into() }],
            temperature: None,
            max_tokens: None,
            tools: vec![],
        };
        assert!(req.tools.is_empty());
    }

    #[test]
    fn llm_response_defaults() {
        let r = LlmResponse {
            text: "hello".into(),
            input_tokens: 5,
            output_tokens: 2,
            model: "claude-3".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        };
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.stop_reason, StopReason::EndTurn);
    }
}
```

- [ ] **Step 2: Run test, verify it fails**

```bash
cargo test -p mur-agent-runtime llm::tests 2>&1 | tail -20
```
Expected: compile errors about missing types.

- [ ] **Step 3: Add new types to `llm/mod.rs`**

After the existing `StreamDelta` struct (around line 50), add:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResult {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

impl Default for StopReason {
    fn default() -> Self {
        StopReason::EndTurn
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolResultEntry {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub enum RichMessage {
    Text { role: String, content: String },
    ToolUse {
        text: Option<String>,
        calls: Vec<ToolCallResult>,
    },
    ToolResults {
        results: Vec<ToolResultEntry>,
    },
}
```

- [ ] **Step 4: Update `LlmRequest` and `LlmResponse`**

Replace the existing structs:

```rust
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub messages: Vec<RichMessage>,  // was Vec<LlmMessage>
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Vec<ToolDef>,         // new
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub tool_calls: Vec<ToolCallResult>,  // new
    pub stop_reason: StopReason,          // new
}
```

Keep the existing `LlmMessage` struct — it is still used internally by `task_runner.rs` temporarily.

- [ ] **Step 5: Update `task_runner.rs` to use `RichMessage::Text`**

In `mur-agent-runtime/src/task_runner.rs`, change line 5 import:
```rust
use crate::llm::{LlmClient, LlmRequest, RichMessage};
```
(Remove `LlmMessage` from the import.)

Replace the message construction in `run_llm` (around lines 430–445):
```rust
// was: messages.push(LlmMessage { role: "system".into(), content: system });
messages.push(RichMessage::Text { role: "system".into(), content: system.clone() });

// was: messages.push(LlmMessage { role: input.role.clone(), content: prompt });
messages.push(RichMessage::Text { role: input.role.clone(), content: prompt.clone() });

let req = LlmRequest {
    messages,
    temperature: None,
    max_tokens: None,
    tools: vec![],   // no tools in single-shot path
};
```

Change the `messages` declaration type:
```rust
let mut messages: Vec<RichMessage> = Vec::new();
```

- [ ] **Step 6: Fix `LlmResponse` construction in `run_llm` and stub providers**

Any place that constructs `LlmResponse` with the old fields must add the two new fields:
```rust
LlmResponse {
    text: ...,
    input_tokens: ...,
    output_tokens: ...,
    model: ...,
    tool_calls: vec![],        // add
    stop_reason: StopReason::EndTurn,  // add
}
```

Search and update:
```bash
grep -rn "LlmResponse {" /Volumes/Firecuda4tb/Projects/mur/mur-agent-runtime/src/
```
Fix every hit.

- [ ] **Step 7: Run tests, verify they pass**

```bash
cargo test -p mur-agent-runtime llm::tests 2>&1 | tail -20
```
Expected: 3 tests pass.

- [ ] **Step 8: Compile check**

```bash
cargo check -p mur-agent-runtime 2>&1 | grep "^error" | head -20
```
Expected: no errors (warnings are ok).

- [ ] **Step 9: Commit**

```bash
git add mur-agent-runtime/src/llm/mod.rs mur-agent-runtime/src/task_runner.rs
git commit -m "feat(llm): add RichMessage, ToolDef, ToolCallResult, StopReason types"
```

---

### Task 2: Anthropic adapter — RichMessage wire format + tool_use parsing

**Files:**
- Modify: `mur-agent-runtime/src/llm/anthropic.rs`

Context: The current `generate()` and `generate_stream()` iterate over `req.messages: Vec<LlmMessage>`. After Task 1 they are `Vec<RichMessage>`. This task updates both methods.

- [ ] **Step 1: Add unit tests at bottom of `anthropic.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{RichMessage, ToolCallResult, ToolDef, ToolResultEntry};
    use serde_json::json;

    fn rich_messages_to_anthropic(
        messages: &[RichMessage],
        tools: &[ToolDef],
    ) -> (Option<String>, Vec<serde_json::Value>, serde_json::Value) {
        // Helper: extract what would be sent — system, convo, and tools array.
        // We replicate the logic we're about to implement.
        let mut system: Option<String> = None;
        let mut convo: Vec<serde_json::Value> = vec![];
        for m in messages {
            match m {
                RichMessage::Text { role, content } if role == "system" => {
                    system = Some(content.clone());
                }
                RichMessage::Text { role, content } => {
                    let r = if role == "agent" { "assistant" } else { role.as_str() };
                    convo.push(json!({"role": r, "content": content}));
                }
                RichMessage::ToolUse { text, calls } => {
                    let mut content = vec![];
                    if let Some(t) = text {
                        if !t.is_empty() { content.push(json!({"type":"text","text":t})); }
                    }
                    for c in calls {
                        content.push(json!({
                            "type": "tool_use",
                            "id": c.call_id,
                            "name": c.tool_name,
                            "input": c.input,
                        }));
                    }
                    convo.push(json!({"role":"assistant","content":content}));
                }
                RichMessage::ToolResults { results } => {
                    let content: Vec<_> = results.iter().map(|r| json!({
                        "type": "tool_result",
                        "tool_use_id": r.call_id,
                        "content": r.content,
                        "is_error": r.is_error,
                    })).collect();
                    convo.push(json!({"role":"user","content":content}));
                }
            }
        }
        let tools_json = if tools.is_empty() {
            json!(null)
        } else {
            json!(tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })).collect::<Vec<_>>())
        };
        (system, convo, tools_json)
    }

    #[test]
    fn serializes_system_to_top_level() {
        let msgs = vec![
            RichMessage::Text { role: "system".into(), content: "Be helpful".into() },
            RichMessage::Text { role: "user".into(), content: "hi".into() },
        ];
        let (sys, convo, _) = rich_messages_to_anthropic(&msgs, &[]);
        assert_eq!(sys.as_deref(), Some("Be helpful"));
        assert_eq!(convo.len(), 1);
        assert_eq!(convo[0]["role"], "user");
    }

    #[test]
    fn serializes_tool_use_turn() {
        let msgs = vec![
            RichMessage::ToolUse {
                text: Some("Running it".into()),
                calls: vec![ToolCallResult {
                    call_id: "toolu_01".into(),
                    tool_name: "bash".into(),
                    input: json!({"command":"ls"}),
                }],
            },
        ];
        let (_, convo, _) = rich_messages_to_anthropic(&msgs, &[]);
        let content = &convo[0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_01");
    }

    #[test]
    fn serializes_tool_results() {
        let msgs = vec![
            RichMessage::ToolResults {
                results: vec![ToolResultEntry {
                    call_id: "toolu_01".into(),
                    content: "file.txt".into(),
                    is_error: false,
                }],
            },
        ];
        let (_, convo, _) = rich_messages_to_anthropic(&msgs, &[]);
        assert_eq!(convo[0]["role"], "user");
        assert_eq!(convo[0]["content"][0]["type"], "tool_result");
        assert_eq!(convo[0]["content"][0]["tool_use_id"], "toolu_01");
        assert_eq!(convo[0]["content"][0]["is_error"], false);
    }

    #[test]
    fn parses_tool_use_response() {
        let body = json!({
            "content": [
                {"type": "text", "text": "I will run ls."},
                {"type": "tool_use", "id": "toolu_01", "name": "bash", "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "I will run ls.");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].call_id, "toolu_01");
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert_eq!(tool_calls[0].input["command"], "ls");
        assert_eq!(stop_reason, crate::llm::StopReason::ToolUse);
    }

    #[test]
    fn parses_end_turn_response() {
        let body = json!({
            "content": [{"type": "text", "text": "Done."}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "Done.");
        assert!(tool_calls.is_empty());
        assert_eq!(stop_reason, crate::llm::StopReason::EndTurn);
    }
}
```

- [ ] **Step 2: Run test, verify it fails**

```bash
cargo test -p mur-agent-runtime llm::anthropic::tests 2>&1 | tail -20
```
Expected: compile error — `parse_response_body` not found.

- [ ] **Step 3: Extract `parse_response_body` helper in `anthropic.rs`**

Add this private function above `impl LlmClient for AnthropicClient`:

```rust
fn parse_response_body(
    v: &serde_json::Value,
) -> Result<(String, Vec<crate::llm::ToolCallResult>, crate::llm::StopReason), LlmError> {
    use crate::llm::{StopReason, ToolCallResult};
    let content = v["content"]
        .as_array()
        .ok_or_else(|| LlmError::InvalidResponse("missing content array".into()))?;

    let text = content
        .iter()
        .filter_map(|b| {
            if b["type"].as_str() == Some("text") {
                b["text"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    let tool_calls: Vec<ToolCallResult> = content
        .iter()
        .filter(|b| b["type"].as_str() == Some("tool_use"))
        .map(|b| ToolCallResult {
            call_id: b["id"].as_str().unwrap_or("").to_string(),
            tool_name: b["name"].as_str().unwrap_or("").to_string(),
            input: b["input"].clone(),
        })
        .collect();

    let stop_reason = match v["stop_reason"].as_str() {
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("end_turn") | None => StopReason::EndTurn,
        Some(other) => StopReason::Other(other.to_string()),
    };

    Ok((text, tool_calls, stop_reason))
}
```

- [ ] **Step 4: Update `generate()` to use `RichMessage` and `parse_response_body`**

Replace the existing `generate()` implementation. Key changes:

1. Replace the `for m in &req.messages { if m.role == "system" ... }` loop with a `match` on `RichMessage`:

```rust
let mut system_chunks: Vec<String> = Vec::new();
let mut convo: Vec<serde_json::Value> = Vec::new();
for m in &req.messages {
    match m {
        RichMessage::Text { role, content } if role == "system" => {
            system_chunks.push(content.clone());
        }
        RichMessage::Text { role, content } => {
            let r = if role == "agent" { "assistant" } else { role.as_str() };
            convo.push(json!({"role": r, "content": content}));
        }
        RichMessage::ToolUse { text, calls } => {
            let mut parts: Vec<serde_json::Value> = vec![];
            if let Some(t) = text {
                if !t.is_empty() {
                    parts.push(json!({"type": "text", "text": t}));
                }
            }
            for c in calls {
                parts.push(json!({
                    "type": "tool_use",
                    "id": c.call_id,
                    "name": c.tool_name,
                    "input": c.input,
                }));
            }
            convo.push(json!({"role": "assistant", "content": parts}));
        }
        RichMessage::ToolResults { results } => {
            let parts: Vec<serde_json::Value> = results.iter().map(|r| json!({
                "type": "tool_result",
                "tool_use_id": r.call_id,
                "content": r.content,
                "is_error": r.is_error,
            })).collect();
            convo.push(json!({"role": "user", "content": parts}));
        }
    }
}
```

2. Add tools to request body (after the `system` block):

```rust
if !req.tools.is_empty() {
    body["tools"] = serde_json::json!(
        req.tools.iter().map(|t| serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        })).collect::<Vec<_>>()
    );
}
```

3. Replace the response parsing at the end with `parse_response_body`:

```rust
let (text, tool_calls, stop_reason) = parse_response_body(&v)?;
let input_tokens = v["usage"]["input_tokens"].as_u64().unwrap_or(0);
let output_tokens = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
Ok(LlmResponse {
    text,
    input_tokens,
    output_tokens,
    model: self.model.clone(),
    tool_calls,
    stop_reason,
})
```

- [ ] **Step 5: Update `generate_stream()` for `RichMessage`**

Apply the same `RichMessage` match loop (same code as Step 4) to replace the existing `for m in &req.messages` loop in `generate_stream`. The SSE response parsing only returns text/thinking deltas (tool_use blocks aren't streamed in Phase 3), so the final `Ok(LlmResponse {...})` at the end of `generate_stream` adds:

```rust
Ok(LlmResponse {
    text,
    input_tokens,
    output_tokens,
    model: self.model.clone(),
    tool_calls: vec![],           // streaming doesn't parse tool_use deltas in Phase 3
    stop_reason: StopReason::EndTurn,
})
```

Add `use crate::llm::{RichMessage, StopReason};` to the import block.

- [ ] **Step 6: Run tests**

```bash
cargo test -p mur-agent-runtime llm::anthropic::tests 2>&1 | tail -20
```
Expected: 5 tests pass.

- [ ] **Step 7: Compile check**

```bash
cargo check -p mur-agent-runtime 2>&1 | grep "^error" | head -20
```

- [ ] **Step 8: Commit**

```bash
git add mur-agent-runtime/src/llm/anthropic.rs
git commit -m "feat(anthropic): RichMessage wire format, tool_use parse, parse_response_body"
```

---

### Task 3: OpenAI adapter — RichMessage wire format + tool_calls parsing

**Files:**
- Modify: `mur-agent-runtime/src/llm/openai.rs`

Context: OpenAI's wire format differs from Anthropic: tools → `{"type":"function","function":{...}}`, tool calls returned in `choices[0].message.tool_calls`, `arguments` is a JSON-*encoded string* (must `serde_json::from_str`), tool results use `role: "tool"`.

- [ ] **Step 1: Add unit tests at bottom of `openai.rs`**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{RichMessage, ToolCallResult, ToolResultEntry};
    use serde_json::json;

    #[test]
    fn parses_tool_calls_response() {
        // arguments is a JSON-encoded string — must be parsed, not used raw
        let body = json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"ls\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert!(text.is_empty());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].call_id, "call_abc");
        assert_eq!(tool_calls[0].tool_name, "bash");
        assert_eq!(tool_calls[0].input["command"], "ls");
        assert_eq!(stop_reason, crate::llm::StopReason::ToolUse);
    }

    #[test]
    fn parses_text_response() {
        let body = json!({
            "choices": [{"message": {"content": "Hello!"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2}
        });
        let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
        assert_eq!(text, "Hello!");
        assert!(tool_calls.is_empty());
        assert_eq!(stop_reason, crate::llm::StopReason::EndTurn);
    }

    #[test]
    fn serializes_tool_results_as_tool_role() {
        let msgs = vec![
            RichMessage::ToolResults {
                results: vec![ToolResultEntry {
                    call_id: "call_abc".into(),
                    content: "file.txt".into(),
                    is_error: false,
                }],
            },
        ];
        let convo = rich_messages_to_openai(&msgs);
        assert_eq!(convo[0]["role"], "tool");
        assert_eq!(convo[0]["tool_call_id"], "call_abc");
        assert_eq!(convo[0]["content"], "file.txt");
    }

    #[test]
    fn serializes_tool_use_as_assistant_with_tool_calls() {
        let msgs = vec![
            RichMessage::ToolUse {
                text: Some("Running".into()),
                calls: vec![ToolCallResult {
                    call_id: "call_abc".into(),
                    tool_name: "bash".into(),
                    input: json!({"command":"ls"}),
                }],
            },
        ];
        let convo = rich_messages_to_openai(&msgs);
        assert_eq!(convo[0]["role"], "assistant");
        assert_eq!(convo[0]["content"], "Running");
        let tc = &convo[0]["tool_calls"][0];
        assert_eq!(tc["id"], "call_abc");
        assert_eq!(tc["function"]["name"], "bash");
    }
}
```

- [ ] **Step 2: Run test, verify compile error**

```bash
cargo test -p mur-agent-runtime llm::openai::tests 2>&1 | tail -10
```

- [ ] **Step 3: Add `parse_response_body` and `rich_messages_to_openai` helpers in `openai.rs`**

```rust
fn parse_response_body(
    v: &serde_json::Value,
) -> Result<(String, Vec<crate::llm::ToolCallResult>, crate::llm::StopReason), LlmError> {
    use crate::llm::{StopReason, ToolCallResult};
    let choice = &v["choices"][0];
    let msg = &choice["message"];

    let text = msg["content"].as_str().unwrap_or("").to_string();

    let tool_calls: Vec<ToolCallResult> = msg["tool_calls"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let call_id = tc["id"].as_str()?.to_string();
                    let tool_name = tc["function"]["name"].as_str()?.to_string();
                    // arguments is a JSON-encoded string — MUST parse
                    let input: serde_json::Value = tc["function"]["arguments"]
                        .as_str()
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    Some(ToolCallResult { call_id, tool_name, input })
                })
                .collect()
        })
        .unwrap_or_default();

    let stop_reason = match choice["finish_reason"].as_str() {
        Some("tool_calls") => StopReason::ToolUse,
        Some("length") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    };

    Ok((text, tool_calls, stop_reason))
}

fn rich_messages_to_openai(messages: &[crate::llm::RichMessage]) -> Vec<serde_json::Value> {
    use crate::llm::RichMessage;
    let mut out = vec![];
    for m in messages {
        match m {
            RichMessage::Text { role, content } => {
                let r = if role == "agent" { "assistant" } else { role.as_str() };
                out.push(json!({"role": r, "content": content}));
            }
            RichMessage::ToolUse { text, calls } => {
                let tcs: Vec<_> = calls.iter().map(|c| {
                    // OpenAI expects arguments as a JSON-encoded string
                    let args = serde_json::to_string(&c.input).unwrap_or_default();
                    json!({
                        "id": c.call_id,
                        "type": "function",
                        "function": {"name": c.tool_name, "arguments": args},
                    })
                }).collect();
                out.push(json!({
                    "role": "assistant",
                    "content": text.as_deref().unwrap_or(""),
                    "tool_calls": tcs,
                }));
            }
            RichMessage::ToolResults { results } => {
                // OpenAI: one message per tool result, role = "tool"
                for r in results {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": r.call_id,
                        "content": r.content,
                    }));
                }
            }
        }
    }
    out
}
```

- [ ] **Step 4: Update `generate()` to use helpers**

Replace the existing message iteration loop with `rich_messages_to_openai`:
```rust
let messages = rich_messages_to_openai(&req.messages);
let mut body = json!({"model": self.model, "messages": messages});
```

Add tools serialization after body construction:
```rust
if !req.tools.is_empty() {
    body["tools"] = json!(
        req.tools.iter().map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            }
        })).collect::<Vec<_>>()
    );
}
```

Replace the response parsing at the end:
```rust
let (text, tool_calls, stop_reason) = parse_response_body(&v)?;
let input_tokens = v["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
let output_tokens = v["usage"]["completion_tokens"].as_u64().unwrap_or(0);
Ok(LlmResponse { text, input_tokens, output_tokens, model: self.model.clone(), tool_calls, stop_reason })
```

- [ ] **Step 5: Update `generate_stream()` for `RichMessage`**

Replace the message iteration with:
```rust
let messages = rich_messages_to_openai(&req.messages);
```

Add same tools block. Final `Ok(LlmResponse {...})` adds:
```rust
tool_calls: vec![],
stop_reason: crate::llm::StopReason::EndTurn,
```

Add `use crate::llm::StopReason;` import.

- [ ] **Step 6: Run tests**

```bash
cargo test -p mur-agent-runtime llm::openai::tests 2>&1 | tail -10
```
Expected: 4 tests pass.

- [ ] **Step 7: Compile check + commit**

```bash
cargo check -p mur-agent-runtime 2>&1 | grep "^error" | head -10
git add mur-agent-runtime/src/llm/openai.rs
git commit -m "feat(openai): RichMessage wire format, tool_calls parse, arguments JSON-string"
```

---

### Task 4: `ToolExecutor` trait + `BashTool`

**Files:**
- Create: `mur-agent-runtime/src/tools/mod.rs`
- Create: `mur-agent-runtime/src/tools/bash.rs`
- Modify: `mur-agent-runtime/src/lib.rs`

- [ ] **Step 1: Add `pub mod tools;` to `lib.rs`**

In `mur-agent-runtime/src/lib.rs`, add after `pub mod task_runner;`:
```rust
pub mod tools;
```

- [ ] **Step 2: Create `tools/mod.rs` with `ToolExecutor` trait**

```rust
//! Tool execution abstraction for the agentic loop.

pub mod bash;

use crate::llm::ToolDef;
use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool execution failed: {0}")]
    Execution(String),
    #[error("unknown tool: {0}")]
    Unknown(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn def(&self) -> ToolDef;
    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError>;
}
```

- [ ] **Step 3: Write failing tests in `bash.rs`**

Create `mur-agent-runtime/src/tools/bash.rs`:

```rust
use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;
use async_trait::async_trait;
use std::path::PathBuf;

pub struct BashTool {
    pub working_dir: PathBuf,
}

impl BashTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolExecutor for BashTool {
    fn name(&self) -> &str { "bash" }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: "bash".into(),
            description: "Execute a shell command and return its stdout and stderr combined.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to run"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        let cmd = input["command"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing command field".into()))?
            .to_string();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&self.working_dir)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await;

        match result {
            Err(_) => Ok(format!("Error: command timed out after 30 seconds")),
            Ok(Err(e)) => Ok(format!("Error: failed to spawn process: {e}")),
            Ok(Ok(output)) => {
                let mut combined = String::new();
                if !output.stdout.is_empty() {
                    combined.push_str(&String::from_utf8_lossy(&output.stdout));
                }
                if !output.stderr.is_empty() {
                    combined.push_str(&String::from_utf8_lossy(&output.stderr));
                }
                // Non-zero exit code surfaces in output, not as Err
                if !output.status.success() {
                    combined.push_str(&format!(
                        "\n[exit code: {}]",
                        output.status.code().unwrap_or(-1)
                    ));
                }
                Ok(combined)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> BashTool {
        BashTool::new(std::env::temp_dir())
    }

    #[tokio::test]
    async fn captures_stdout() {
        let t = tool();
        let out = t.execute(serde_json::json!({"command": "echo hello"})).await.unwrap();
        assert!(out.contains("hello"), "got: {out}");
    }

    #[tokio::test]
    async fn captures_stderr() {
        let t = tool();
        let out = t.execute(serde_json::json!({"command": "echo err >&2"})).await.unwrap();
        assert!(out.contains("err"), "got: {out}");
    }

    #[tokio::test]
    async fn nonzero_exit_in_output_not_err() {
        let t = tool();
        let result = t.execute(serde_json::json!({"command": "exit 1"})).await;
        assert!(result.is_ok(), "should be Ok, got: {result:?}");
        let out = result.unwrap();
        assert!(out.contains("exit code"), "got: {out}");
    }

    #[tokio::test]
    async fn missing_command_is_invalid_input() {
        let t = tool();
        let result = t.execute(serde_json::json!({})).await;
        assert!(matches!(result, Err(ToolError::InvalidInput(_))));
    }
}
```

- [ ] **Step 4: Run tests, verify they fail**

```bash
cargo test -p mur-agent-runtime tools::bash::tests 2>&1 | tail -20
```
Expected: compile errors (module exists but no impl yet — wait, the impl IS in the code above; tests should compile).

Actually run them immediately since the code is written above:

- [ ] **Step 5: Run tests**

```bash
cargo test -p mur-agent-runtime tools::bash::tests 2>&1 | tail -20
```
Expected: 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/lib.rs mur-agent-runtime/src/tools/
git commit -m "feat(tools): ToolExecutor trait + BashTool (stdout, stderr, timeout, exit code)"
```

---

### Task 5: `HitlConfig.max_iterations` in `mur-common`

**Files:**
- Modify: `mur-common/src/agent.rs`

- [ ] **Step 1: Write failing test**

In `mur-common/src/agent.rs`, find the existing `HitlConfig` tests section (or add a new one). Add:

```rust
#[cfg(test)]
mod hitl_tests {
    use super::*;

    #[test]
    fn hitl_config_max_iterations_defaults_none() {
        let c: HitlConfig = serde_yaml::from_str("timeout_secs: 60").unwrap();
        assert_eq!(c.max_iterations, None);
    }

    #[test]
    fn hitl_config_max_iterations_explicit() {
        let c: HitlConfig = serde_yaml::from_str("timeout_secs: 60\nmax_iterations: 5").unwrap();
        assert_eq!(c.max_iterations, Some(5));
    }
}
```

- [ ] **Step 2: Run test, verify it fails**

```bash
cargo test -p mur-common hitl_tests 2>&1 | tail -10
```
Expected: compile error — `max_iterations` field not found.

- [ ] **Step 3: Add field to `HitlConfig`**

Find the existing `HitlConfig` struct (around line 852):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HitlConfig {
    #[serde(default = "default_hitl_timeout_secs")]
    pub timeout_secs: u32,
    #[serde(default)]
    pub max_iterations: Option<u32>,   // add this
}
```

- [ ] **Step 4: Run tests + compile check + commit**

```bash
cargo test -p mur-common hitl_tests 2>&1 | tail -10
cargo check -p mur-agent-runtime 2>&1 | grep "^error" | head -10
git add mur-common/src/agent.rs
git commit -m "feat(common): HitlConfig.max_iterations (None → runtime default 10)"
```

---

### Task 6: `HitlDecision` type + supervisor update + hitl.rs reason param

**Files:**
- Create: `mur-agent-runtime/src/hitl.rs`
- Modify: `mur-agent-runtime/src/lib.rs`
- Modify: `mur-agent-runtime/src/supervisor.rs`
- Modify: `mur-hub-gui/src-tauri/src/hitl.rs`

Context: `pending_approvals` currently sends `bool`. This task changes it to send `HitlDecision { allow, reason }`.

- [ ] **Step 1: Create `mur-agent-runtime/src/hitl.rs`**

```rust
//! Shared HITL types used by both supervisor and task_runner.

#[derive(Debug, Clone)]
pub struct HitlDecision {
    pub allow: bool,
    pub reason: Option<String>,
}
```

- [ ] **Step 2: Add `pub mod hitl;` to `lib.rs`**

In `mur-agent-runtime/src/lib.rs`, add:
```rust
pub mod hitl;
```

- [ ] **Step 3: Update `supervisor.rs` — change pending_approvals type + HitlRespondHandler**

Find and update the `pending_approvals` declaration and type (line 269):

**Before:**
```rust
let pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>> =
    Arc::new(Mutex::new(HashMap::new()));
```

**After:**
```rust
let pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>> =
    Arc::new(Mutex::new(HashMap::new()));
```

Update `HitlRespondHandler` struct and the two places that declare `Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>` (lines 604 and 639/680):

```rust
struct HitlRespondHandler {
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>>,
}
```

Update `HitlRespondHandler::handle()` to extract reason and send `HitlDecision` (replacing line 633 `let _ = tx.send(allow);`):

```rust
let allow = p["allow"].as_bool().ok_or_else(|| {
    crate::protocol::a2a_server::HandlerError::InvalidParams("missing allow".into())
})?;
let reason = p["reason"].as_str().map(str::to_string);
let _ = tx.send(crate::hitl::HitlDecision { allow, reason });
```

Update `HitlTestRequestHandler` struct's `pending_approvals` type similarly:

```rust
struct HitlTestRequestHandler {
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>>,
    notifier: tokio::sync::mpsc::Sender<serde_json::Value>,
}
```

Update `HitlTestRequestHandler::handle()` line 654 (`let (tx, _rx) = oneshot::channel::<bool>();`) to:
```rust
let (tx, _rx) = oneshot::channel::<crate::hitl::HitlDecision>();
```

Update `build_dispatcher` signature (line 680):
```rust
fn build_dispatcher(
    ...
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>>,
) -> Dispatcher {
```

- [ ] **Step 4: Update existing HITL tests in `supervisor.rs`**

Find the `hitl_tests` module (around line 1009). The existing test creates `oneshot::channel::<bool>()`. Update to use `HitlDecision`:

```rust
// hitl_respond_resolves_pending
let (tx, rx) = oneshot::channel::<crate::hitl::HitlDecision>();
// ...
let decision = rx.await.unwrap();
assert!(decision.allow);
assert!(decision.reason.is_none());

// hitl_respond_unknown_id_returns_error — no change needed (it doesn't send)
```

Add a new test:
```rust
#[tokio::test]
async fn hitl_respond_carries_reason() {
    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = oneshot::channel::<crate::hitl::HitlDecision>();
    pending.lock().unwrap().insert("test-id".into(), tx);
    let handler = HitlRespondHandler { pending_approvals: pending.clone() };
    handler
        .handle(Some(json!({"hitl_id": "test-id", "allow": false, "reason": "too risky"})))
        .await
        .unwrap();
    let decision = rx.await.unwrap();
    assert!(!decision.allow);
    assert_eq!(decision.reason.as_deref(), Some("too risky"));
}
```

- [ ] **Step 5: Update Tauri `hitl.rs` to add `reason` param**

Replace `mur-hub-gui/src-tauri/src/hitl.rs` entirely:

```rust
use mur_core::a2a_dial::{DialMode, dial_method};
use serde_json::json;

#[tauri::command]
pub fn agent_hitl_respond(
    name: String,
    hitl_id: String,
    allow: bool,
    reason: Option<String>,
) -> Result<(), String> {
    let home = crate::mur_home_path();
    dial_method(
        &home,
        &name,
        "tool/hitl_respond",
        json!({ "hitl_id": hitl_id, "allow": allow, "reason": reason }),
        DialMode::RequireRunning,
    )
    .map(|_| ())
    .map_err(|e| format!("{e:#}"))
}
```

- [ ] **Step 6: Run supervisor tests + compile check**

```bash
cargo test -p mur-agent-runtime hitl_tests 2>&1 | tail -20
cargo check -p mur-agent-runtime 2>&1 | grep "^error" | head -20
cargo check -p mur-hub-gui --manifest-path mur-hub-gui/src-tauri/Cargo.toml 2>&1 | grep "^error" | head -10
```
Expected: supervisor tests pass (3 tests), no errors.

- [ ] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/hitl.rs mur-agent-runtime/src/lib.rs \
        mur-agent-runtime/src/supervisor.rs mur-hub-gui/src-tauri/src/hitl.rs
git commit -m "feat(hitl): HitlDecision type, reason field in HitlRespondHandler + Tauri command"
```

---

### Task 7: `TaskRunner` agentic fields + `run_agentic_loop` + supervisor wiring

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs`
- Modify: `mur-agent-runtime/src/supervisor_runner.rs`
- Modify: `mur-agent-runtime/src/supervisor.rs`

Context: `pending_approvals` is created in `supervisor.rs` and currently only passed to `build_dispatcher`. We need to create it *before* calling `build_provider_runner` so it can be threaded into `TaskRunner`. The `sock_notif_tx` is already available at that point.

- [ ] **Step 1: Write failing tests at bottom of `task_runner.rs`**

```rust
#[cfg(test)]
mod agentic_tests {
    use super::*;
    use crate::hitl::HitlDecision;
    use crate::llm::{LlmError, LlmRequest, LlmResponse, RichMessage, StopReason, ToolCallResult};
    use std::sync::Arc;

    // Stub LLM that returns a configurable sequence of responses
    struct SequenceLlm {
        responses: tokio::sync::Mutex<Vec<LlmResponse>>,
    }
    impl SequenceLlm {
        fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
            Arc::new(Self { responses: tokio::sync::Mutex::new(responses) })
        }
    }
    #[async_trait::async_trait]
    impl crate::llm::LlmClient for SequenceLlm {
        fn model_name(&self) -> &str { "test" }
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let mut q = self.responses.lock().await;
            if q.is_empty() {
                Ok(LlmResponse {
                    text: "done".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                    model: "test".into(),
                    tool_calls: vec![],
                    stop_reason: StopReason::EndTurn,
                })
            } else {
                Ok(q.remove(0))
            }
        }
    }

    fn make_runner_with_hitl() -> (
        Arc<TaskRunner>,
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>>>,
        tokio::sync::mpsc::Receiver<serde_json::Value>,
    ) {
        let pending: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = tokio::sync::mpsc::channel(16);
        let runner = Arc::new(
            TaskRunner::with_llm(SequenceLlm::new(vec![]))
                .with_pending_approvals(pending.clone())
                .with_notifier(notif_tx),
        );
        (runner, pending, notif_rx)
    }

    fn tool_call_response(call_id: &str, command: &str) -> LlmResponse {
        LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![ToolCallResult {
                call_id: call_id.into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": command}),
            }],
            stop_reason: StopReason::ToolUse,
        }
    }

    fn end_turn_response(text: &str) -> LlmResponse {
        LlmResponse {
            text: text.into(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        }
    }

    #[tokio::test]
    async fn loop_ends_on_end_turn_no_tools() {
        // A TaskRunner with no tools uses run_llm (not run_agentic_loop)
        let runner = TaskRunner::new_stub_echo();
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text { text: "ping".into() }],
            },
            context_task_id: None,
        };
        let outcome = runner.run_sync(spec).await;
        assert!(matches!(outcome, TaskOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn loop_deny_returns_error_tool_result_in_history() {
        // LLM: first call → tool call, second call → end turn
        // HITL gate: deny the tool call
        // Expected: task completes (LLM was called twice; deny shows up as error tool_result)
        let (pending, notif_rx_) = {
            let pa: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>>> =
                Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            (pa, rx)
        };
        let _ = notif_rx_; // not checking notifications in this test

        let responses = vec![
            tool_call_response("id-1", "rm -rf /"),
            end_turn_response("ok, skipped"),
        ];
        let llm = SequenceLlm::new(responses);
        let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
        let runner = Arc::new(
            TaskRunner::with_llm(llm)
                .with_pending_approvals(pending.clone())
                .with_notifier(notif_tx)
                .with_hitl_timeout_secs(1),  // 1s timeout → auto-deny
        );
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text { text: "do something".into() }],
            },
            context_task_id: None,
        };
        // With 1s timeout and no responder, tool call auto-denies.
        // Second LLM call → EndTurn → Completed.
        let outcome = runner.run_sync(spec).await;
        assert!(matches!(outcome, TaskOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn loop_max_iterations_yields_failed() {
        // LLM: always returns tool_call → loop hits max_iterations
        let responses: Vec<LlmResponse> = (0..12)
            .map(|i| tool_call_response(&format!("id-{i}"), "echo loop"))
            .collect();
        let llm = SequenceLlm::new(responses);
        let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
        let pa: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<HitlDecision>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let runner = Arc::new(
            TaskRunner::with_llm(llm)
                .with_pending_approvals(pa)
                .with_notifier(notif_tx)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(3),  // low cap for test
        );
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text { text: "loop".into() }],
            },
            context_task_id: None,
        };
        let outcome = runner.run_sync(spec).await;
        assert!(
            matches!(outcome, TaskOutcome::Failed(_)),
            "expected Failed, got {outcome:?}"
        );
        if let TaskOutcome::Failed(task) = outcome {
            let err = task.error.unwrap();
            assert!(err.message.contains("iteration"), "got: {}", err.message);
        }
    }
}
```

- [ ] **Step 2: Run tests, verify compile errors**

```bash
cargo test -p mur-agent-runtime agentic_tests 2>&1 | tail -30
```
Expected: compile errors about missing methods/fields.

- [ ] **Step 3: Add new fields to `TaskRunner` struct**

In `task_runner.rs`, update the `TaskRunner` struct. After `hook_cancel: Option<CancellationToken>`, add:

```rust
pending_approvals: Option<Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>>>>,
notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
hitl_timeout_secs: u32,
max_iterations: u32,
```

In `with_backend()`, set the new fields to defaults:
```rust
pending_approvals: None,
notifier: None,
hitl_timeout_secs: 300,
max_iterations: 10,
```

Add builder methods after `with_hook_chain`:
```rust
pub fn with_pending_approvals(
    mut self,
    pa: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>>>,
) -> Self {
    self.pending_approvals = Some(pa);
    self
}

pub fn with_notifier(mut self, tx: tokio::sync::mpsc::Sender<serde_json::Value>) -> Self {
    self.notifier = Some(tx);
    self
}

pub fn with_hitl_timeout_secs(mut self, secs: u32) -> Self {
    self.hitl_timeout_secs = secs;
    self
}

pub fn with_max_iterations(mut self, n: u32) -> Self {
    self.max_iterations = n;
    self
}
```

- [ ] **Step 4: Add `run_agentic_loop` and `handle_tool_call` to `TaskRunner`**

Add these two private methods after `run_llm`. Use the `?` operator and return `Result<Message, TaskError>`.

```rust
async fn run_agentic_loop(
    &self,
    task_id: &str,
    client: &dyn LlmClient,
    system_prompt: String,
    input: &Message,
    _sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
) -> Result<Message, TaskError> {
    use crate::llm::{LlmRequest, RichMessage, StopReason};
    let tool_defs: Vec<_> = self.tools_for_loop().iter().map(|t| t.def()).collect();
    let prompt = text_of(input);

    let mut history: Vec<RichMessage> = vec![
        RichMessage::Text { role: "system".into(), content: system_prompt },
        RichMessage::Text { role: input.role.clone(), content: prompt },
    ];

    for _ in 0..self.max_iterations {
        let req = LlmRequest {
            messages: history.clone(),
            tools: tool_defs.clone(),
            temperature: None,
            max_tokens: None,
        };
        let resp = client.generate(req).await.map_err(|e| task_error("llm_error", format!("{e}"), true))?;

        // Push assistant turn into history
        history.push(RichMessage::ToolUse {
            text: if resp.text.is_empty() { None } else { Some(resp.text.clone()) },
            calls: resp.tool_calls.clone(),
        });

        if resp.tool_calls.is_empty() || resp.stop_reason == StopReason::EndTurn {
            return Ok(Message {
                role: "agent".into(),
                parts: vec![mur_common::a2a::MessagePart::Text { text: resp.text }],
            });
        }

        let mut results = vec![];
        for call in &resp.tool_calls {
            let result = self.handle_tool_call(task_id, call).await;
            results.push(result);
        }
        history.push(RichMessage::ToolResults { results });
    }

    Err(task_error(
        "max_iterations_exceeded",
        format!("Agent reached tool call iteration limit ({})", self.max_iterations),
        false,
    ))
}

async fn handle_tool_call(
    &self,
    _task_id: &str,
    call: &crate::llm::ToolCallResult,
) -> crate::llm::ToolResultEntry {
    use crate::llm::ToolResultEntry;

    // 1. pre_tool_use hook chain
    if let (Some(chain), Some(ctx), Some(cancel)) =
        (&self.hook_chain, &self.hook_ctx, &self.hook_cancel)
    {
        let hook_call = crate::hooks::ToolCall {
            tool_name: call.tool_name.clone(),
            mcp_server: None,
            call_id: call.call_id.clone(),
            input: call.input.clone(),
        };
        match chain.pre_tool_use(ctx, &hook_call, cancel).await {
            Ok(crate::hooks::Decision::Deny { reason }) => {
                return ToolResultEntry {
                    call_id: call.call_id.clone(),
                    content: format!("Tool call blocked by policy. Reason: {reason}"),
                    is_error: true,
                };
            }
            Ok(_) => {} // Allow or AskUser → proceed to HITL gate
            Err(_) => {} // Hook error → proceed
        }
    }

    // 2. HITL gate via pending_approvals
    let decision = if let (Some(pa), Some(notifier)) = (&self.pending_approvals, &self.notifier) {
        let hitl_id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel::<crate::hitl::HitlDecision>();
        pa.lock().await.insert(hitl_id.clone(), tx);
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool/approval_needed",
            "params": {
                "hitl_id": hitl_id,
                "tool_name": call.tool_name,
                "tool_input": call.input,
                "prompt": format!("Agent wants to run `{}`", call.tool_name),
                "timeout_ms": (self.hitl_timeout_secs as u64) * 1000,
            }
        });
        let _ = notifier.send(notification).await;
        match tokio::time::timeout(
            std::time::Duration::from_secs(self.hitl_timeout_secs as u64),
            rx,
        ).await {
            Ok(Ok(d)) => d,
            _ => {
                pa.lock().await.remove(&hitl_id);
                crate::hitl::HitlDecision { allow: false, reason: Some("Tool call timed out".into()) }
            }
        }
    } else {
        // No HITL gate configured — allow by default
        crate::hitl::HitlDecision { allow: true, reason: None }
    };

    if !decision.allow {
        let reason_str = decision.reason.as_deref().unwrap_or("");
        let msg = if reason_str.is_empty() {
            "Tool call denied by user.".to_string()
        } else {
            format!("Tool call denied by user. Reason: {reason_str}")
        };
        return ToolResultEntry { call_id: call.call_id.clone(), content: msg, is_error: true };
    }

    // 3. Execute tool
    let tools = self.tools_for_loop();
    let executor = tools.iter().find(|t| t.name() == call.tool_name);
    match executor {
        None => ToolResultEntry {
            call_id: call.call_id.clone(),
            content: format!("Unknown tool: {}", call.tool_name),
            is_error: true,
        },
        Some(exec) => match exec.execute(call.input.clone()).await {
            Ok(output) => ToolResultEntry { call_id: call.call_id.clone(), content: output, is_error: false },
            Err(e) => ToolResultEntry { call_id: call.call_id.clone(), content: format!("Error: {e}"), is_error: true },
        },
    }
}

fn tools_for_loop(&self) -> Vec<Box<dyn crate::tools::ToolExecutor>> {
    vec![Box::new(crate::tools::bash::BashTool::new(
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".mur"),
    ))]
}
```

Note: `tools_for_loop()` always returns `BashTool` — no per-instance tool list in Phase 3.

- [ ] **Step 5: Update `run_sync_inner` to branch on HITL setup**

In `run_sync_inner`, update the `RunnerBackend::Llm` arm:

```rust
RunnerBackend::Llm(client) => {
    if self.pending_approvals.is_some() {
        // Agentic path — has HITL gate
        let (system, _fired) = self.assemble_system_prompt(&text_of(&spec.input));
        // Apply hook chain on_prompt_submit (copy the existing logic from run_llm)
        let system = /* same hook chain logic as run_llm — extract to helper or duplicate */ system;
        self.run_agentic_loop(&id, client.as_ref(), system, &spec.input, sink).await
    } else {
        self.run_llm(&id, client.as_ref(), &spec.input, sink).await
    }
}
```

The `system` assembly with hook chain is duplicated from `run_llm`. Extract the system-prompt + hook-chain logic into a private helper `prepare_system_prompt(&self, input: &Message) -> Result<String, TaskError>` so both paths use it. This is the right refactor since `run_agentic_loop` also needs it.

**Extracted helper:**
```rust
async fn prepare_system_prompt(&self, input: &Message) -> Result<String, TaskError> {
    let prompt = text_of(input);
    let (system, _fired) = self.assemble_system_prompt(&prompt);
    if let (Some(chain), Some(ctx), Some(cancel)) =
        (&self.hook_chain, &self.hook_ctx, &self.hook_cancel)
    {
        // ... identical hook chain logic from run_llm ...
        // return Ok(patched_system)
    } else {
        Ok(system)
    }
}
```

Then `run_llm` and `run_agentic_loop` both call `self.prepare_system_prompt(input).await?`.

- [ ] **Step 6: Update `supervisor.rs` — create pending_approvals before runner**

In `supervisor.rs`, move the `pending_approvals` creation ABOVE the `build_provider_runner` call, and clone it for passing to the runner:

**Before** (around line 259–276):
```rust
let (runner, llm_for_companion) = crate::supervisor_runner::build_provider_runner(
    force_echo, &profile, ...
).await?;
let pending_approvals: Arc<...> = Arc::new(Mutex::new(HashMap::new()));
let dispatcher = Arc::new(build_dispatcher(&profile_arc, &runner, &mur_home, sock_notif_tx.clone(), pending_approvals));
```

**After:**
```rust
let pending_approvals: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>>> =
    Arc::new(tokio::sync::Mutex::new(HashMap::new()));

let (runner, llm_for_companion) = crate::supervisor_runner::build_provider_runner(
    force_echo,
    &profile,
    runtime_skills.clone(),
    skills_cfg.clone(),
    &hook_chain,
    &hook_ctx,
    &hook_cancel,
    pending_approvals.clone(),     // NEW
    sock_notif_tx.clone(),          // NEW
    profile.inner.hitl.timeout_secs,  // NEW
).await?;
let dispatcher = Arc::new(build_dispatcher(
    &profile_arc, &runner, &mur_home, sock_notif_tx.clone(), pending_approvals
));
```

- [ ] **Step 7: Update `supervisor_runner.rs` — thread new params through `build_provider_runner` and `build_runner`**

Update `build_runner` signature to accept new params:

```rust
pub fn build_runner(
    client: Arc<dyn LlmClient>,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
    pending_approvals: Option<Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>>>>,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    hitl_timeout_secs: u32,
) -> Arc<TaskRunner> {
    let mut runner = TaskRunner::with_llm(client)
        .with_system_prompt(base_system_prompt)
        .with_skills(skills)
        .with_skills_cfg(skills_cfg)
        .with_hitl_timeout_secs(hitl_timeout_secs);
    if let (Some(chain), Some(ctx), Some(cancel)) = (hook_chain, hook_ctx, hook_cancel) {
        runner = runner.with_hook_chain(chain, ctx, cancel);
    }
    if let Some(pa) = pending_approvals {
        runner = runner.with_pending_approvals(pa);
    }
    if let Some(notif) = notifier {
        runner = runner.with_notifier(notif);
    }
    Arc::new(runner)
}
```

Update `build_provider_runner` signature to accept and pass through the new params. Update the `build` closure inside `build_provider_runner`:

```rust
let build = |client: Arc<dyn LlmClient>| {
    let r = crate::supervisor_runner::build_runner(
        client.clone(),
        profile.system_prompt.clone(),
        runtime_skills.clone(),
        skills_cfg.clone(),
        Some(Arc::new(hook_chain.clone())),
        Some(hook_ctx.clone()),
        Some(hook_cancel.clone()),
        Some(pending_approvals.clone()),  // NEW
        Some(notifier.clone()),           // NEW
        hitl_timeout_secs,                // NEW
    );
    (r, Some(client))
};
```

- [ ] **Step 8: Run agentic tests**

```bash
cargo test -p mur-agent-runtime agentic_tests 2>&1 | tail -40
```
Expected: 3 tests pass.

- [ ] **Step 9: Run full test suite**

```bash
cargo test -p mur-agent-runtime 2>&1 | tail -20
```
Expected: all existing tests still pass + new tests pass.

- [ ] **Step 10: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs \
        mur-agent-runtime/src/supervisor.rs \
        mur-agent-runtime/src/supervisor_runner.rs
git commit -m "feat(runtime): run_agentic_loop + handle_tool_call + HITL gate wiring"
```

---

### Task 8: Hub UI — `HitlCard` deny-with-reason + i18n

**Files:**
- Modify: `mur-hub-gui/ui/src/components/HitlCard.tsx`
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`
- Modify: `mur-hub-gui/ui/src/i18n/zh-TW.ts`

Context: `HitlCard.tsx` currently has `[Allow]` and `[Deny]` buttons. The `agent_hitl_respond` Tauri command now accepts `reason: Option<String>`. We add a `[Deny ▾]` button that expands a reason textarea before confirming.

- [ ] **Step 1: Add i18n keys to `en.ts`**

Before the closing `} as const;` in `en.ts`, add:

```typescript
  // ── HITL deny-with-reason ──
  "hitl.denyWithReason": "Deny with reason",
  "hitl.denyReason": "Reason",
  "hitl.reasonPlaceholder": "Why are you denying this? (optional)",
  "hitl.confirmDeny": "Confirm Deny",
```

- [ ] **Step 2: Add i18n keys to `zh-TW.ts`**

Before the closing `};` in `zh-TW.ts`, add:

```typescript
  // ── HITL deny-with-reason ──
  "hitl.denyWithReason": "附理由拒絕",
  "hitl.denyReason": "理由",
  "hitl.reasonPlaceholder": "為什麼要拒絕？（選填）",
  "hitl.confirmDeny": "確認拒絕",
```

- [ ] **Step 3: Update `HitlCard.tsx`**

Replace the file with the updated implementation:

```tsx
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { HitlRequest } from "../types";

interface Props {
  request: HitlRequest;
}

export function HitlCard({ request }: Props) {
  const timeoutSecs = Math.floor(request.timeout_ms / 1000);
  const [remaining, setRemaining] = useState(timeoutSecs);
  const [responded, setResponded] = useState<"allowed" | "denied" | "timeout" | null>(null);
  const [busy, setBusy] = useState(false);
  const [denyExpanded, setDenyExpanded] = useState(false);
  const [reason, setReason] = useState("");
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    intervalRef.current = setInterval(() => {
      setRemaining((r) => {
        if (r <= 1) {
          clearInterval(intervalRef.current!);
          setResponded("timeout");
          return 0;
        }
        return r - 1;
      });
    }, 1000);
    return () => clearInterval(intervalRef.current!);
  }, []);

  async function respond(allow: boolean, denyReason?: string) {
    if (responded || busy) return;
    setBusy(true);
    clearInterval(intervalRef.current!);
    try {
      await invoke("agent_hitl_respond", {
        name: request.agent,
        hitlId: request.hitl_id,
        allow,
        reason: denyReason ?? null,
      });
      setResponded(allow ? "allowed" : "denied");
    } catch {
      setResponded(allow ? "allowed" : "denied");
    } finally {
      setBusy(false);
    }
  }

  const inputSummary = Object.entries(request.tool_input)
    .slice(0, 2)
    .map(([k, v]) => `${k}: ${String(v).slice(0, 40)}`)
    .join(", ");

  if (responded === "timeout") {
    return (
      <div className="hitl-card hitl-card--resolved hitl-card--timeout">
        <span className="hitl-card__label">⏱ Timed out — request auto-denied</span>
      </div>
    );
  }
  if (responded === "allowed") {
    return (
      <div className="hitl-card hitl-card--resolved hitl-card--allowed">
        <span className="hitl-card__label">✓ Allowed</span>
      </div>
    );
  }
  if (responded === "denied") {
    return (
      <div className="hitl-card hitl-card--resolved hitl-card--denied">
        <span className="hitl-card__label">✕ Denied</span>
      </div>
    );
  }

  const mins = Math.floor(remaining / 60);
  const secs = remaining % 60;
  const countdown = `${mins}:${String(secs).padStart(2, "0")}`;

  return (
    <div className="hitl-card">
      <div className="hitl-card__header">
        <span className="hitl-card__icon">⏸</span>
        <span className="hitl-card__title">Approval needed</span>
        <span className="hitl-card__timer">{countdown}</span>
      </div>
      <div className="hitl-card__prompt">{request.prompt}</div>
      {inputSummary && (
        <div className="hitl-card__input">{inputSummary}</div>
      )}
      <div className="hitl-card__actions">
        <button
          className="hitl-card__btn hitl-card__btn--allow"
          onClick={() => respond(true)}
          disabled={busy}
        >
          Allow
        </button>
        {/* Single-click deny — no reason */}
        <button
          className="hitl-card__btn hitl-card__btn--deny"
          onClick={() => respond(false)}
          disabled={busy || denyExpanded}
        >
          Deny
        </button>
        {/* Deny with reason — expands textarea */}
        <button
          className="hitl-card__btn hitl-card__btn--deny-reason"
          onClick={() => setDenyExpanded((v) => !v)}
          disabled={busy}
          aria-expanded={denyExpanded}
        >
          Deny ▾
        </button>
      </div>
      {denyExpanded && (
        <div className="hitl-card__reason-form">
          <textarea
            className="hitl-card__reason-input"
            placeholder="Why are you denying this? (optional)"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            rows={2}
          />
          <button
            className="hitl-card__btn hitl-card__btn--confirm-deny"
            onClick={() => respond(false, reason || undefined)}
            disabled={busy}
          >
            Confirm Deny
          </button>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 4: TypeScript check**

```bash
cd mur-hub-gui/ui && npx tsc --noEmit 2>&1 | head -20
```
Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/HitlCard.tsx \
        mur-hub-gui/ui/src/i18n/en.ts \
        mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): HitlCard deny-with-reason, expand textarea, i18n keys"
```

---

## Final check

After all 8 tasks, run:

```bash
# Runtime tests
cargo test -p mur-agent-runtime 2>&1 | tail -20

# Common tests  
cargo test -p mur-common 2>&1 | tail -10

# TypeScript
cd mur-hub-gui/ui && npx tsc --noEmit 2>&1 | head -20
```

All tests green → hand off to `superpowers:finishing-a-development-branch`.
