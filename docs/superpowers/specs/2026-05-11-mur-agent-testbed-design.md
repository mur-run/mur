# MUR Agent Testbed — Design Spec

**Date:** 2026-05-11  
**Status:** Approved, pending implementation plan  
**Approach:** Real Process + Trace Side Channel (Option A)

---

## Background

MUR Agent runtime (`mur-agent-runtime`) already has extensive per-feature integration tests (companion, bridge, scheduler, sandbox, B0 rules, etc.) and a `Companion` test harness with `MockClock` / `FakeNotifier` / `StubLlm`. What is missing is a **developer-facing interactive sandbox** that lets engineers send messages to a real agent and observe internal state (B0 rule decisions, LLM I/O, hook chain timing, agent snapshot) without writing test code.

Industry research (2025-2026) confirms this is the "Reproducible Reality" layer of the four-tier testing pyramid (Block Engineering / Anthropic). The testbed complements — but does not replace — existing unit tests and the B2 red-team gate.

---

## Goals

- `mur agent testbed <name>` — CLI REPL against a real agent process with live trace output
- `/testbed` page in mur-web — browser UI via mur serve SSE stream
- OTel GenAI Semantic Conventions-aligned trace format (future-proof for Arize Phoenix / Langfuse)
- Zero impact on production paths: `--testbed` flag is opt-in; trace emit failures are silent

## Non-Goals

- Replacing existing `cargo test` integration tests
- Full multi-turn evaluation / pass@k harness (separate effort)
- Agent-tests-agent (LangWatch Scenario pattern) — future phase

---

## Architecture Overview

```
mur agent testbed <name>
        │
        ├─ spawns ──► mur-agent-runtime --profile <name> --testbed --trace-fd 3  start
        │                      │
        │              stdio (JSON-RPC)   fd 3 (TraceSpan NDJSON)
        │                      │                │
        └─ REPL reads both ────┴────────────────┘

mur serve /api/testbed/start
        │
        └─ same spawn pattern → SSE pushes trace + reply events to mur-web /testbed
```

---

## §1 — TraceEvent Data Model

Aligned with [OTel GenAI Semantic Conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/gen-ai-spans/) so traces can be exported to observability backends without schema translation.

**File:** `mur-agent-runtime/src/testbed/trace.rs`

```rust
#[derive(Serialize)]
pub struct TraceSpan {
    pub ts_ms: u64,
    pub span_id: String,                 // random 8-char hex
    pub parent_span_id: Option<String>,
    pub duration_ms: Option<u64>,        // None if span still open
    pub attrs: SpanAttrs,
}

#[derive(Serialize)]
#[serde(tag = "gen_ai.operation.name", rename_all = "snake_case")]
pub enum SpanAttrs {
    /// gen_ai.operation.name = "chat" — LLM request/response
    Chat {
        #[serde(rename = "gen_ai.request.model")]
        model: String,
        prompt: String,
        response: Option<String>,
        input_tokens: u32,
        output_tokens: u32,
    },
    /// gen_ai.operation.name = "execute_tool" — hook step or B0 decision
    ExecuteTool {
        #[serde(rename = "gen_ai.tool.name")]
        name: String,
        outcome: String,           // "ok" | "skipped" | "deny"
        b0_rule: Option<u8>,
        b0_verdict: Option<String>, // "allow" | "deny"
        b0_context: Option<String>,
    },
    /// gen_ai.operation.name = "agent_snapshot" — per-tick state
    AgentSnapshot {
        task_queue_len: usize,
        proactive_paused: bool,
        picker_weights: Vec<(String, f32)>,
    },
}
```

Spans are written as newline-delimited JSON to fd 3 (or `$MUR_TRACE_FD`). Each line is a complete `TraceSpan`. The CLI and mur serve both consume this stream independently.

### SpanGuard (RAII duration tracking)

```rust
// mur-agent-runtime/src/testbed/guard.rs
pub struct SpanGuard<'a> {
    emitter: &'a TraceEmitter,
    span: TraceSpan,
    start: Instant,
}

impl Drop for SpanGuard<'_> {
    fn drop(&mut self) {
        self.span.duration_ms = Some(self.start.elapsed().as_millis() as u64);
        self.emitter.emit_sync(&self.span); // blocking fd write; EBADF → silent
    }
}
```

---

## §2 — CLI REPL Interface

**Subcommand:** `mur agent testbed <agent-name>`  
**Implementation file:** `mur-core/src/cmd/agent/testbed.rs` (new, ≤ 200 lines)

### Startup

```
$ mur agent testbed my-agent
[testbed] Starting my-agent in sandbox mode…
[testbed] Profile: ~/.mur/agents/my-agent/profile.yaml
[testbed] Trace fd: 3  |  LLM: anthropic/claude-sonnet-4-6
[testbed] Type a message, or :help for commands.
────────────────────────────────────────────────
you>
```

The REPL runs three concurrent tasks:
1. Read user input → wrap as A2A `message/send` → write to runtime stdin
2. Read runtime stdout → parse JSON-RPC response → print agent reply
3. Read fd 3 → parse `TraceSpan` → render per verbosity level

### Default Output Format

```
you> 幫我查一下明天的天氣

── hook chain ─────────────────────────────────
  ✓ b0_prefilter          0.3ms
  ✓ secret_redaction      0.1ms
  ✓ mcp_signature_verify  0.2ms

── llm ────────────────────────────────────────
  model: claude-sonnet-4-6   in: 842 tok  out: 67 tok  120ms

agent> 明天台北天氣晴，高溫 28°C。
```

### Magic Commands (`:` prefix)

| Command | Description |
|---------|-------------|
| `:llm` | Expand full prompt + raw LLM response from last turn |
| `:rules` | List all B0 decisions from last turn (rule id, verdict, context) |
| `:state` | Show current AgentSnapshot (task queue, picker weights) |
| `:replay <id>` | Feed `session/recordings/<id>.jsonl` into the agent |
| `:verbose` | Toggle all spans with parent/child indentation |
| `:reset` | Restart agent subprocess (clear conversation, keep profile) |
| `:help` | List all commands |

---

## §3 — mur serve API + mur-web Testbed Page

### API Routes

**Implementation file:** `mur-core/src/server/routes/testbed.rs` (new)

```
POST   /api/testbed/start       → spawn agent subprocess, return { session_id }
POST   /api/testbed/:id/send    → send message, return { message_id }
GET    /api/testbed/:id/stream  → SSE stream (trace + reply events)
POST   /api/testbed/:id/command → execute magic command (:rules, :state, :replay…)
DELETE /api/testbed/:id         → close session, kill subprocess
```

Session state: `HashMap<SessionId, TestbedSession>` held in server AppState.  
`TestbedSession` owns: `Child` process handle + trace fd reader task + SSE sender.

### SSE Event Format

All events share one stream, differentiated by `event:` field:

```
event: trace
data: {"gen_ai.operation.name":"execute_tool","gen_ai.tool.name":"b0_prefilter","outcome":"ok","duration_ms":0.3}

event: trace
data: {"gen_ai.operation.name":"chat","model":"claude-sonnet-4-6","input_tokens":842,"output_tokens":67,"duration_ms":120}

event: reply
data: {"text":"明天台北天氣晴，高溫 28°C。","message_id":"m_abc123"}

event: snapshot
data: {"task_queue_len":0,"proactive_paused":false,"picker_weights":[["greet-1",1.0]]}
```

### mur-web Page Layout (`/testbed`)

```
┌─────────────────────────────────────────────────────────────┐
│ Testbed  [my-agent ▾]                    [Reset] [Verbose ▾]│
├──────────────────────────┬──────────────────────────────────┤
│  CONVERSATION            │  TRACE PANEL                      │
│                          │  ─── Hook Chain ──────────────── │
│  you: 幫我查天氣          │  ✓ b0_prefilter        0.3ms     │
│                          │  ✓ secret_redaction    0.1ms     │
│  agent: 明天台北晴…       │  ─── LLM ─────────────────────  │
│                          │  model: claude-sonnet-4-6        │
│                          │  in: 842 tok  out: 67 tok  120ms │
│                          │  [展開 Prompt ▾]                  │
│                          │  ─── B0 Rules ─────────────────  │
│                          │  rule 3: spotlight  ✓ allow       │
│                          │  ─── Agent State ──────────────  │
│                          │  tasks: 0  proactive: active      │
│  ┌──────────────────────┐│                                   │
│  │ 輸入訊息…        [送] ││                                   │
│  └──────────────────────┘│                                   │
└──────────────────────────┴───────────────────────────────────┘
```

---

## §4 — Runtime `--testbed` Mode

### New Flag

```
mur-agent-runtime --profile <name> --testbed [--trace-fd <n>] start
```

`--trace-fd` defaults to 3. Without `--testbed`, the `TraceEmitter` is `None`; all emit calls are `if self.trace.is_some()` no-ops.

### Module Structure

```
mur-agent-runtime/src/testbed/
├── mod.rs      → TraceEmitter (holds BufWriter<File> from fd)
├── trace.rs    → TraceSpan / SpanAttrs types
└── guard.rs    → SpanGuard (RAII duration)
```

`TraceEmitter` is injected as `Arc<Option<TraceEmitter>>` into `Supervisor`, then threaded down to each instrumented layer.

### Instrumentation Points (minimum viable set)

| Layer | File | Instrumentation |
|-------|------|-----------------|
| LLM call | `llm/anthropic.rs`, `llm/openai.rs` | `Chat` span wraps HTTP request; response fills `output_tokens` + `duration_ms` |
| Hook chain step | `hooks/mod.rs` `run_chain()` | `SpanGuard` around each step |
| B0 decision | `communication_policy.rs` `check()` | `ExecuteTool` span after verdict |
| Per-tick snapshot | `supervisor.rs` tick loop | `AgentSnapshot` at end of each tick |

All emit calls: fail-silent (EBADF on non-testbed fd → silently ignored).

---

## §5 — Testing Strategy & CI Integration

### Four-Tier Pyramid

**Tier 1 — Deterministic (every PR, < 1 min)**

Tests trace emit correctness without real LLM:

```rust
// mur-agent-runtime/tests/testbed_trace.rs
#[tokio::test]
async fn b0_deny_emits_execute_tool_span() {
    let (read_fd, write_fd) = pipe();
    let emitter = TraceEmitter::from_fd(write_fd);
    // trigger B0 rule 3 via StubLlm → read trace fd
    // assert b0_verdict == "deny", rule == 3
}
```

Also tests: CLI magic command parsing, SSE event serialization round-trip.

**Tier 2 — Reproducible Reality (PR gate, record/playback)**

`RecordingProvider` wraps `LlmClient`:
- **Record mode:** calls real LLM, stores `hash(request) → response` in `tests/fixtures/recordings/`
- **Playback mode:** `lookup(hash(request))` → returns fixture, zero API cost
- **Hash key:** `sha256(model + full_messages_json)` — includes full conversation history so multi-turn replay is deterministic per conversation context

CI always runs playback mode. Fixtures are committed to the repo. `session/recordings/<id>.jsonl` can seed new fixture sets.

**Tier 3 — Probabilistic (weekly nightly, pass@k)**

```yaml
# .github/workflows/agent-eval.yml
on:
  schedule:
    - cron: '0 2 * * 1'   # Monday 02:00 UTC
  workflow_dispatch:

jobs:
  eval:
    steps:
      - run: cargo test --test testbed_e2e -- --nocapture
        env:
          MUR_EVAL_K: 5
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
```

Pass thresholds: pass@k ≥ 0.8, pass^k ≥ 0.6. Failure auto-opens GitHub Issue tagged `eval-regression`.

**Tier 4 — Red-Team (extends existing B2)**

Two new Promptfoo testbed cases on top of the existing 15-case PR gate:
- Slack bridge incoming message → indirect prompt injection (highest risk per InjecAgent research)
- A2A envelope → excessive agency attempt

### CI Matrix

| Trigger | Tests | Budget |
|---------|-------|--------|
| PR push | Tier 1 + Promptfoo 15 cases | < 2 min |
| PR merge | Tier 1 + Tier 2 record/playback | < 5 min |
| Weekly nightly | Tier 3 pass@k + InjecAgent-200 | ~ 30 min |

---

## File Inventory

### New files
- `mur-agent-runtime/src/testbed/mod.rs`
- `mur-agent-runtime/src/testbed/trace.rs`
- `mur-agent-runtime/src/testbed/guard.rs`
- `mur-agent-runtime/tests/testbed_trace.rs`
- `mur-agent-runtime/tests/fixtures/recordings/` (directory, populated by RecordingProvider)
- `mur-core/src/cmd/agent/testbed.rs`
- `mur-core/src/server/routes/testbed.rs`
- `mur-web/src/app/testbed/page.tsx` (or equivalent path)
- `.github/workflows/agent-eval.yml`

### Modified files
- `mur-agent-runtime/src/main.rs` — add `--testbed` / `--trace-fd` flags
- `mur-agent-runtime/src/supervisor.rs` — inject `TraceEmitter`, emit `AgentSnapshot`
- `mur-agent-runtime/src/hooks/mod.rs` — wrap `run_chain()` with `SpanGuard`
- `mur-agent-runtime/src/communication_policy.rs` — emit B0 `ExecuteTool` span
- `mur-agent-runtime/src/llm/anthropic.rs`, `llm/openai.rs` — emit `Chat` span
- `mur-core/src/cmd/agent/mod.rs` — register `testbed` subcommand
- `mur-core/src/server/routes/mod.rs` — register `/api/testbed` routes

---

## Industry Research Summary

Key sources informing this design:

- **Block Engineering Testing Pyramid** — 4-tier model, RecordingProvider pattern, no real LLM in CI
- **Anthropic "Demystifying Evals" (2026-01)** — pass@k vs pass^k, grade outcomes not paths
- **OTel GenAI Semantic Conventions** — span schema alignment for future observability backend integration
- **InjecAgent (ACL 2024)** — Slack/Telegram bridge identified as highest indirect prompt injection risk
- **A2A Protocol Security (2025-05)** — 60-100% data exfiltration rate without proper controls; MUR's Ed25519 auth mitigates some risks
- **OpenHands SDK (2025-11)** — event-sourced state + deterministic replay, aligns with MUR's existing `session/recordings/*.jsonl`
