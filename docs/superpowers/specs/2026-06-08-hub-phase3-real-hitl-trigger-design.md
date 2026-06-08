# Hub Phase 3 — Real HITL Trigger Design

## Goal

Wire a tool-use loop into `TaskRunner` so actual LLM tool calls pause for Hub approval before execution. This closes the Phase 2 HITL story: the plumbing (`pending_approvals`, `HitlCard`) is already in place; Phase 3 makes the LLM produce real tool calls and routes them through that gate.

## Scope

- **In:** Bash tool only; Anthropic + OpenAI-compatible providers; deny-with-reason; max_iterations cap
- **Out:** File read/write tools; Ollama tool support; MCP tool dispatch; agent profile tool toggle

## Architecture

```
LLM Provider (Anthropic / OpenAI-compat)
        │
        │  LlmRequest { messages: Vec<RichMessage>, tools: Vec<ToolDef> }
        ▼
  provider adapter          ← converts to/from wire format
        │
        │  LlmResponse { text: String, tool_calls: Vec<ToolCallResult>, stop_reason }
        ▼
  run_agentic_loop           ← private method on TaskRunner
        │
        ├─ stop_reason == EndTurn  → return final Message
        │
        └─ stop_reason == ToolUse → for each tool_call:
                │
                ├─ pre_tool_use hook chain
                │       ├─ Allow / AskUser → proceed to HITL gate
                │       └─ Deny(reason)   → error tool_result (skip HITL)
                │
                ├─ pending_approvals gate  (Phase 2 plumbing)
                │       ├─ allow   → ToolExecutor::execute()
                │       └─ deny(reason) → error tool_result
                │
                └─ append tool_results → next LLM turn (loop)
```

`run_sync_inner` is unchanged except for a branch: when `self.tools` is non-empty it calls `run_agentic_loop`; otherwise it calls the existing `run_llm`. `BashTool` is always injected into `TaskRunner::new()` — no profile toggle in Phase 3.

**Hub not connected:** If the Hub is closed, `tool/approval_needed` is sent but nobody receives it. After `hitl.timeout_secs` the gate auto-denies. This is the intended safety behavior — headless agents cannot execute tools without a connected Hub.

## New Types

### `mur-agent-runtime/src/llm/mod.rs`

```rust
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,  // JSON Schema object
}

pub struct ToolCallResult {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}
```

**`LlmRequest.messages` changes from `Vec<LlmMessage>` to `Vec<RichMessage>`** — this is the key backward-compatible change that lets the agentic loop carry tool history. `LlmMessage` callers wrap their message as `RichMessage::Text { role, content }`.

`LlmResponse` gains `tool_calls: Vec<ToolCallResult>` (default empty) and `stop_reason: StopReason`. `text: String` remains — it is empty string when `stop_reason == ToolUse`.

### `mur-agent-runtime/src/llm/mod.rs` — loop history type

`LlmMessage` is text-only and cannot carry tool-use history. The agentic loop builds `Vec<RichMessage>`:

```rust
pub enum RichMessage {
    Text { role: String, content: String },
    ToolUse {
        // assistant turn that included tool calls
        text: Option<String>,        // may be empty
        calls: Vec<ToolCallResult>,
    },
    ToolResults {
        results: Vec<ToolResultEntry>,
    },
}

pub struct ToolResultEntry {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}
```

Each provider adapter converts `Vec<RichMessage>` → its own JSON structure. `RichMessage::Text` with role "system" carries the system prompt. The initial user input is wrapped as `RichMessage::Text { role: "user", content: ... }`.

### `mur-agent-runtime/src/tools/`

```rust
// tools/mod.rs
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn def(&self) -> ToolDef;
    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError>;
}

// tools/bash.rs
pub struct BashTool;
// - extracts input["command"] as &str
// - runs via tokio::process::Command, shell=true
// - working directory: agent home dir (e.g. ~/.mur/agents/<name>/)
// - 30s timeout
// - captures stdout + stderr, returns combined string
// - non-zero exit code is surfaced in output, not as Err
// - stdin is closed
```

### `mur-common/src/agent.rs`

`HitlConfig` gains `max_iterations: Option<u32>` (default `None` → runtime uses 10).

### HITL decision type (`mur-agent-runtime/src/supervisor.rs`)

```rust
pub struct HitlDecision {
    pub allow: bool,
    pub reason: Option<String>,
}
```

`HitlRespondHandler` updated to carry the `reason` field through the oneshot channel.

## Provider Adapters

### Anthropic (`llm/anthropic.rs`)

**Request:** prepend system prompt as `RichMessage::Text { role: "system", ... }` → Anthropic top-level `system` field. Serialize `tools` as Anthropic tool schema array.

**Response:** parse `content[]` for `type == "tool_use"` blocks:
```json
{ "type": "tool_use", "id": "toolu_01...", "name": "bash", "input": { "command": "ls" } }
```
→ `ToolCallResult { call_id: "toolu_01...", tool_name: "bash", input: {...} }`

`stop_reason: "tool_use"` → `StopReason::ToolUse`.

**Tool result message** (next turn, role = "user"):
```json
{
  "role": "user",
  "content": [{
    "type": "tool_result",
    "tool_use_id": "toolu_01...",
    "content": "<stdout>",
    "is_error": false
  }]
}
```

**RichMessage → Anthropic wire format:**
- `Text { role: "system", .. }` → top-level `system` field
- `Text { role: "user"|"assistant", .. }` → `{ role, content: string }`
- `ToolUse { text, calls }` → `{ role: "assistant", content: [text_block?, ...tool_use_blocks] }`
- `ToolResults { results }` → `{ role: "user", content: [...tool_result_blocks] }`

### OpenAI-compatible (`llm/openai.rs`)

**Request:** serialize `tools` as OpenAI function-calling format:
```json
{ "type": "function", "function": { "name": "bash", "description": "...", "parameters": {...} } }
```

**Response:** parse `choices[0].message.tool_calls`:
```json
{ "id": "call_abc", "function": { "name": "bash", "arguments": "{\"command\":\"ls\"}" } }
```
Note: `arguments` is a **JSON-encoded string** — must be parsed with `serde_json::from_str(arguments)` to obtain the `input` object.

→ `ToolCallResult { call_id: "call_abc", tool_name: "bash", input: {...} }`

`finish_reason: "tool_calls"` → `StopReason::ToolUse`.

**Tool result message** (next turn):
```json
{ "role": "tool", "tool_call_id": "call_abc", "content": "<stdout>" }
```

**RichMessage → OpenAI wire format:**
- `Text { role, content }` → `{ role, content }` (system/user/assistant all string content)
- `ToolUse { text, calls }` → `{ role: "assistant", content: text_or_null, tool_calls: [...] }`
- `ToolResults { results }` → one `{ role: "tool", tool_call_id, content }` message per result

### Ollama

No changes. Text-only, tool support varies too much by model for Phase 3.

## run_agentic_loop

```
fn run_agentic_loop(task_id, client, system_prompt, initial_user_input, tools, sink):
  tool_defs = tools.map(def)
  history: Vec<RichMessage> = [
    RichMessage::Text { role: "system", content: system_prompt },
    RichMessage::Text { role: "user",   content: initial_user_input },
  ]
  for _ in 0..max_iterations:
    resp = client.complete(LlmRequest { messages: history.clone(), tools: tool_defs.clone() })
    history.push(RichMessage::ToolUse { text: Some(resp.text), calls: resp.tool_calls.clone() })
    if resp.tool_calls.is_empty():
      return build_message(resp.text)   // EndTurn
    results = []
    for call in resp.tool_calls:
      result = handle_tool_call(task_id, call, tools).await
      results.push(result)
    history.push(RichMessage::ToolResults { results })
  return Err(MaxIterationsExceeded)
  // surfaces as Task failed; Hub chat shows "Agent reached tool call limit (10)"
```

### handle_tool_call

1. `pre_tool_use` hook chain — may return:
   - `Decision::Allow` → proceed
   - `Decision::AskUser { prompt, .. }` → route to HITL gate using hook's prompt string
   - `Decision::Deny { reason }` → return error tool_result immediately (skip HITL gate)
2. `pending_approvals` gate — insert oneshot tx, send `tool/approval_needed` notification
   - Payload: `{ hitl_id, tool_name: "bash", tool_input: { command: "..." }, prompt, timeout_ms }`
3. Await approval with `hitl.timeout_secs` deadline
4. `allow=true` → call `ToolExecutor::execute(input)`
5. `allow=false` → return `ToolResultEntry { is_error: true, content: "Tool call denied by user. Reason: <reason>" }`
6. timeout → return `ToolResultEntry { is_error: true, content: "Tool call timed out" }`

## Hub UI Changes

### `HitlCard.tsx`

Deny flow gains a reason field. Clicking `[Deny]` immediately sends with empty reason. The `[Deny ▾]` variant expands a reason input before sending — for users who want to explain:

```
[Allow]  [Deny]  [Deny ▾]
                  ┌────────────────────────┐
                  │ Reason (optional)      │
                  └────────────────────────┘
                  [Confirm Deny]
```

`[Deny]` → single click, reason = null, immediate.
`[Deny ▾]` → expands reason box, requires [Confirm Deny] to send.

`agent_hitl_respond` Tauri command gains `reason: Option<String>` parameter.

### i18n keys (en + zh-TW)

- `hitl.denyReason`
- `hitl.confirmDeny`
- `hitl.reasonPlaceholder`
- `hitl.denyWithReason`

## Testing

| Layer | File | Covers |
|---|---|---|
| Unit | `tools/bash.rs` | stdout, stderr, timeout, non-zero exit |
| Unit | `llm/anthropic.rs` | tool_use block parse, RichMessage → Anthropic wire format |
| Unit | `llm/openai.rs` | tool_calls parse (arguments JSON string), RichMessage → OpenAI wire format |
| Unit | `task_runner.rs` | loop ends on EndTurn |
| Unit | `task_runner.rs` | deny returns error tool_result with reason |
| Unit | `task_runner.rs` | max_iterations surfaces as Task failed |
| Unit | `task_runner.rs` | HITL timeout auto-denies |
| Unit | `task_runner.rs` | no-tools path unchanged (run_llm) |
| Unit | `task_runner.rs` | AskUser from hook → routes to HITL gate |
| Unit | `supervisor.rs` | HitlDecision carries reason field |
| Integration | `supervisor.rs` | approve flow: tool executes, result in history |
| TypeScript | `HitlCard.tsx` | single-click deny sends empty reason |
| TypeScript | `HitlCard.tsx` | deny-with-reason expands input, confirm sends reason |
| TypeScript | `HitlCard.tsx` | allow path unchanged |

~14 new tests, all TDD.

## Files Changed

| File | Action |
|---|---|
| `mur-agent-runtime/src/llm/mod.rs` | Modify — add ToolDef, ToolCallResult, StopReason, RichMessage, ToolResultEntry; change LlmRequest.messages to Vec<RichMessage>; extend LlmResponse |
| `mur-agent-runtime/src/llm/anthropic.rs` | Modify — RichMessage→wire conversion, tool schema serialization, tool_use parsing |
| `mur-agent-runtime/src/llm/openai.rs` | Modify — RichMessage→wire conversion, function-calling serialization, tool_calls parsing (arguments JSON string) |
| `mur-agent-runtime/src/tools/mod.rs` | Create — ToolExecutor trait |
| `mur-agent-runtime/src/tools/bash.rs` | Create — BashTool |
| `mur-agent-runtime/src/task_runner.rs` | Modify — run_agentic_loop, handle_tool_call, branch in run_sync_inner, BashTool injected in new() |
| `mur-agent-runtime/src/supervisor.rs` | Modify — HitlDecision type, reason in HitlRespondHandler |
| `mur-common/src/agent.rs` | Modify — HitlConfig.max_iterations |
| `mur-hub-gui/src-tauri/src/hitl.rs` | Modify — agent_hitl_respond gains reason param |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Modify — single-click deny + deny-with-reason variant |
| `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` | Modify — 4 new hitl keys |
