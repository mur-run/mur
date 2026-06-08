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
        │  LlmRequest { messages, tools: Vec<ToolDef> }
        ▼
  provider adapter          ← converts to/from wire format
        │
        │  LlmResponse { text?, tool_calls: Vec<ToolCallResult>, stop_reason }
        ▼
  run_agentic_loop           ← private method on TaskRunner
        │
        ├─ stop_reason == EndTurn  → return final Message
        │
        └─ stop_reason == ToolUse → for each tool_call:
                │
                ├─ pre_tool_use hook chain
                │
                ├─ pending_approvals gate  (Phase 2 plumbing)
                │       ├─ allow   → ToolExecutor::execute()
                │       └─ deny(reason) → error tool_result
                │
                └─ append tool_results → next LLM turn (loop)
```

`run_sync_inner` is unchanged except for a branch: when `self.tools` is non-empty it calls `run_agentic_loop`; otherwise it calls the existing `run_llm`.

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

`LlmRequest` gains `tools: Vec<ToolDef>` (default empty).  
`LlmResponse` gains `tool_calls: Vec<ToolCallResult>` (default empty) and `stop_reason: StopReason`.

Backward-compatible: existing callers with no tools continue to work unchanged.

### `mur-agent-runtime/src/llm/mod.rs` — loop history type

`LlmMessage` is currently text-only (`role + content: String`) and cannot carry structured tool-use history. The agentic loop uses a richer internal type:

```rust
pub enum RichMessage {
    Text { role: String, content: String },
    ToolUse {
        // assistant turn that included tool calls
        text: Option<String>,
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

`LlmClient::complete` signature gains an overload that accepts `Vec<RichMessage>` so providers can convert to their wire format. The existing `Vec<LlmMessage>` path remains for single-shot calls. Each provider's adapter converts `RichMessage` → its own JSON structure when building the request body.

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
// - 30s timeout
// - captures stdout + stderr, returns combined string
// - non-zero exit code is surfaced in output, not as Err
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

**Request:** serialize `tools` as Anthropic tool schema array.

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

### OpenAI-compatible (`llm/openai.rs`)

**Request:** serialize `tools` as OpenAI function-calling format:
```json
{ "type": "function", "function": { "name": "bash", "description": "...", "parameters": {...} } }
```

**Response:** parse `choices[0].message.tool_calls`:
```json
{ "id": "call_abc", "function": { "name": "bash", "arguments": "{\"command\":\"ls\"}" } }
```
→ `ToolCallResult { call_id: "call_abc", tool_name: "bash", input: {...} }`

`finish_reason: "tool_calls"` → `StopReason::ToolUse`.

**Tool result message** (next turn):
```json
{ "role": "tool", "tool_call_id": "call_abc", "content": "<stdout>" }
```

### Ollama

No changes. Text-only, tool support varies too much by model for Phase 3.

## run_agentic_loop

```
fn run_agentic_loop(task_id, client, initial_input, tools, sink):
  tool_defs = tools.map(def)
  history = [initial_input]
  for i in 0..max_iterations:
    resp = client.complete(LlmRequest { messages: history, tools: tool_defs })
    history.push(assistant_message(resp))
    if resp.tool_calls.is_empty():
      return build_message(resp.text)
    for call in resp.tool_calls:
      result = handle_tool_call(task_id, call, tools)
      // result is either ok(output) or error(reason)
    history.push(tool_results_message(results))
  return Err(MaxIterationsExceeded)
```

### handle_tool_call

1. `pre_tool_use` hook chain — existing path, may return `Decision::Deny`
2. `pending_approvals` gate — insert oneshot tx, send `tool/approval_needed` notification (Hub shows HitlCard)
3. Await approval with `hitl.timeout_secs` deadline
4. `allow=true` → call `ToolExecutor::execute(input)`
5. `allow=false` → return error tool_result with reason string
6. timeout → return error tool_result "Tool call timed out"

## Hub UI Changes

### `HitlCard.tsx`

Deny flow gains a two-step confirmation with an optional reason field:

```
[Allow]  [Deny ▾]
         ┌────────────────────────┐
         │ Reason (optional)      │
         └────────────────────────┘
         [Confirm Deny]
```

`agent_hitl_respond` Tauri command gains `reason: Option<String>` parameter.

### i18n keys (en + zh-TW)

- `hitl.denyReason`
- `hitl.confirmDeny`
- `hitl.reasonPlaceholder`

## Testing

| Layer | File | Covers |
|---|---|---|
| Unit | `tools/bash.rs` | stdout, stderr, timeout, non-zero exit |
| Unit | `llm/anthropic.rs` | tool_use block parse, tool_result format |
| Unit | `llm/openai.rs` | tool_calls parse, tool role message format |
| Unit | `task_runner.rs` | loop ends on EndTurn |
| Unit | `task_runner.rs` | deny returns error tool_result with reason |
| Unit | `task_runner.rs` | max_iterations triggers error |
| Unit | `task_runner.rs` | HITL timeout auto-denies |
| Unit | `task_runner.rs` | no-tools path unchanged (run_llm) |
| Unit | `supervisor.rs` | HitlDecision carries reason field |
| Integration | `supervisor.rs` | approve flow: tool executes, result in history |
| TypeScript | `HitlCard.tsx` | deny expands reason input, confirm sends reason |
| TypeScript | `HitlCard.tsx` | allow path unchanged |

~12–14 new tests, all TDD.

## Files Changed

| File | Action |
|---|---|
| `mur-agent-runtime/src/llm/mod.rs` | Modify — add ToolDef, ToolCallResult, StopReason; extend LlmRequest/Response |
| `mur-agent-runtime/src/llm/anthropic.rs` | Modify — add tool schema serialization, tool_use parsing |
| `mur-agent-runtime/src/llm/openai.rs` | Modify — add function-calling serialization, tool_calls parsing |
| `mur-agent-runtime/src/tools/mod.rs` | Create — ToolExecutor trait |
| `mur-agent-runtime/src/tools/bash.rs` | Create — BashTool |
| `mur-agent-runtime/src/task_runner.rs` | Modify — run_agentic_loop, handle_tool_call, branch in run_sync_inner |
| `mur-agent-runtime/src/supervisor.rs` | Modify — HitlDecision type, reason in HitlRespondHandler |
| `mur-common/src/agent.rs` | Modify — HitlConfig.max_iterations |
| `mur-hub-gui/src-tauri/src/hitl.rs` | Modify — agent_hitl_respond gains reason param |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` | Modify — deny reason input, two-step confirm |
| `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` | Modify — 3 new hitl keys |
