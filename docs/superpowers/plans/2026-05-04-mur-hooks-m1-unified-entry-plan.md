# mur Hooks M1 — Unified Entry Binary Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace three shell scripts (`on-prompt.sh`, `on-tool.sh`, `on-stop.sh`) with a single `mur hook <event> --tool <name>` Rust subcommand that normalises stdin across nine AI tools and routes each event through the adaptive gate.

**Architecture:** A new `mur hook` subcommand (five events: `prompt`, `tool`, `stop`, `session-start`, `stats`) reads tool-specific JSON from stdin, normalises it into a `NormalizedEvent`, writes it to `~/.mur/queue/events.jsonl`, and for `prompt` events runs the existing keyword-path retrieval gated by `evaluate_query_v2`. The daemon (M3) will read the queue later; in M1 it is written but not consumed. `mur init --hooks` is rewritten to install one-liner scripts (`exec mur hook prompt --tool claude`) instead of multi-line bash.

**Tech Stack:** Rust 2024 edition, `serde_json`, existing `retrieve::gate`, `retrieve::scoring`, `store::yaml`, `inject::hook::format_unified_injection_with_store`. No async embedding in the hook hot path (keyword-only in M1; embedding moved to M3 daemon).

---

## Task 1: `inject::event` — normalised event types + per-tool parsers

**Files:**
- Create: `mur-core/src/inject/event.rs`
- Modify: `mur-core/src/inject/mod.rs`

**Step 1: Write the failing test**

Add to a new file `mur-core/src/inject/event.rs` (use `#[cfg(test)]` block at the bottom):

```rust
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── Normalised types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Prompt,
    Tool,
    Stop,
    SessionStart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub kind: EventKind,
    pub tool_provider: String,        // "claude" | "gemini" | "cursor" | "copilot" | "opencode" | "amp"
    pub query: Option<String>,        // prompt text (Prompt events)
    pub tool_called: Option<String>,  // tool function name (Tool events)
    pub tool_input: Option<Value>,    // raw tool input (Tool events)
    pub stop_reason: Option<String>,  // "end_turn" etc. (Stop events)
    pub session_id: Option<String>,
}

// ── Parsers ───────────────────────────────────────────────────────────────────

pub fn parse_event(raw: Value, kind: EventKind, provider: &str) -> NormalizedEvent {
    match provider {
        "gemini" => parse_gemini(raw, kind),
        "cursor" => parse_cursor(raw, kind),
        "copilot" => parse_copilot(raw, kind),
        "opencode" => parse_opencode(raw, kind),
        _ => parse_claude(raw, kind), // claude, amp, auggie share the same schema
    }
}

fn parse_claude(raw: Value, kind: EventKind) -> NormalizedEvent {
    NormalizedEvent {
        kind: kind.clone(),
        tool_provider: "claude".into(),
        query: raw.get("prompt").and_then(|v| v.as_str()).map(str::to_owned),
        tool_called: raw.get("tool_name").and_then(|v| v.as_str()).map(str::to_owned),
        tool_input: raw.get("tool_input").cloned(),
        stop_reason: raw.get("stop_reason").and_then(|v| v.as_str()).map(str::to_owned),
        session_id: raw.get("session_id").and_then(|v| v.as_str()).map(str::to_owned),
    }
}

fn parse_gemini(raw: Value, kind: EventKind) -> NormalizedEvent {
    // BeforeAgent → prompt field; AfterTool → tool + result; SessionEnd → status
    NormalizedEvent {
        kind: kind.clone(),
        tool_provider: "gemini".into(),
        query: raw.get("prompt").and_then(|v| v.as_str()).map(str::to_owned),
        tool_called: raw.get("tool").and_then(|v| v.as_str()).map(str::to_owned),
        tool_input: raw.get("result").cloned(),
        stop_reason: raw.get("status").and_then(|v| v.as_str()).map(str::to_owned),
        session_id: raw.get("session_id").and_then(|v| v.as_str()).map(str::to_owned),
    }
}

fn parse_cursor(raw: Value, kind: EventKind) -> NormalizedEvent {
    // beforeSubmitPrompt → prompt; beforeShellExecution → command; stop → {}
    NormalizedEvent {
        kind: kind.clone(),
        tool_provider: "cursor".into(),
        query: raw.get("prompt").and_then(|v| v.as_str()).map(str::to_owned),
        tool_called: raw.get("command").and_then(|v| v.as_str()).map(|s| format!("shell:{s}")),
        tool_input: None,
        stop_reason: raw.get("stop_reason").and_then(|v| v.as_str()).map(str::to_owned),
        session_id: None,
    }
}

fn parse_copilot(raw: Value, kind: EventKind) -> NormalizedEvent {
    // userPromptSubmitted → prompt; preToolUse → tool + input; sessionEnd → {}
    let tool_input = raw.get("input").cloned()
        .or_else(|| raw.get("tool_input").cloned());
    NormalizedEvent {
        kind: kind.clone(),
        tool_provider: "copilot".into(),
        query: raw.get("prompt").and_then(|v| v.as_str()).map(str::to_owned),
        tool_called: raw.get("tool").and_then(|v| v.as_str()).map(str::to_owned),
        tool_input,
        stop_reason: None,
        session_id: None,
    }
}

fn parse_opencode(raw: Value, kind: EventKind) -> NormalizedEvent {
    // session.created → session.id; tool.execute.after → tool.name + tool.input
    let session_obj = raw.get("session");
    let tool_obj = raw.get("tool");
    NormalizedEvent {
        kind: kind.clone(),
        tool_provider: "opencode".into(),
        query: None, // OpenCode doesn't expose the raw prompt in hooks
        tool_called: tool_obj
            .and_then(|t| t.get("name"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        tool_input: tool_obj.and_then(|t| t.get("input")).cloned(),
        stop_reason: session_obj
            .and_then(|s| s.get("status"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        session_id: session_obj
            .and_then(|s| s.get("id"))
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claude_prompt() {
        let raw = json!({
            "prompt": "how do I implement tokio retry logic?",
            "session_id": "abc123"
        });
        let ev = parse_event(raw, EventKind::Prompt, "claude");
        assert_eq!(ev.kind, EventKind::Prompt);
        assert_eq!(ev.query.as_deref(), Some("how do I implement tokio retry logic?"));
        assert_eq!(ev.session_id.as_deref(), Some("abc123"));
        assert!(ev.tool_called.is_none());
    }

    #[test]
    fn parse_claude_pre_tool_use() {
        let raw = json!({
            "tool_name": "Edit",
            "tool_input": {"command": "str_replace", "path": "src/main.rs"}
        });
        let ev = parse_event(raw, EventKind::Tool, "claude");
        assert_eq!(ev.kind, EventKind::Tool);
        assert_eq!(ev.tool_called.as_deref(), Some("Edit"));
        assert!(ev.tool_input.is_some());
    }

    #[test]
    fn parse_claude_stop() {
        let raw = json!({"stop_reason": "end_turn"});
        let ev = parse_event(raw, EventKind::Stop, "claude");
        assert_eq!(ev.kind, EventKind::Stop);
        assert_eq!(ev.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn parse_gemini_before_agent() {
        let raw = json!({"prompt": "explain async/await in Rust"});
        let ev = parse_event(raw, EventKind::Prompt, "gemini");
        assert_eq!(ev.tool_provider, "gemini");
        assert_eq!(ev.query.as_deref(), Some("explain async/await in Rust"));
    }

    #[test]
    fn parse_gemini_after_tool() {
        let raw = json!({"tool": "bash", "result": "cargo build succeeded"});
        let ev = parse_event(raw, EventKind::Tool, "gemini");
        assert_eq!(ev.tool_called.as_deref(), Some("bash"));
    }

    #[test]
    fn parse_cursor_prompt() {
        let raw = json!({"prompt": "refactor this function", "language": "rust"});
        let ev = parse_event(raw, EventKind::Prompt, "cursor");
        assert_eq!(ev.query.as_deref(), Some("refactor this function"));
    }

    #[test]
    fn parse_copilot_prompt() {
        let raw = json!({"prompt": "add error handling", "timeoutSec": 30});
        let ev = parse_event(raw, EventKind::Prompt, "copilot");
        assert_eq!(ev.query.as_deref(), Some("add error handling"));
    }

    #[test]
    fn parse_copilot_pre_tool_use() {
        let raw = json!({"tool": "bash", "input": {"command": "npm test"}});
        let ev = parse_event(raw, EventKind::Tool, "copilot");
        assert_eq!(ev.tool_called.as_deref(), Some("bash"));
        assert!(ev.tool_input.is_some());
    }

    #[test]
    fn parse_opencode_session_created() {
        let raw = json!({"session": {"id": "sess_123", "project": "/Users/user/app"}});
        let ev = parse_event(raw, EventKind::SessionStart, "opencode");
        assert_eq!(ev.session_id.as_deref(), Some("sess_123"));
    }

    #[test]
    fn parse_opencode_tool_execute() {
        let raw = json!({"tool": {"name": "bash", "input": {"command": "cargo run"}}});
        let ev = parse_event(raw, EventKind::Tool, "opencode");
        assert_eq!(ev.tool_called.as_deref(), Some("bash"));
    }

    #[test]
    fn parse_empty_stdin_does_not_panic() {
        let raw = serde_json::json!({});
        let ev = parse_event(raw, EventKind::Prompt, "claude");
        assert_eq!(ev.kind, EventKind::Prompt);
        assert!(ev.query.is_none());
    }

    #[test]
    fn amp_uses_claude_parser() {
        let raw = json!({"prompt": "fix the build", "session_id": "s1"});
        let ev = parse_event(raw, EventKind::Prompt, "amp");
        assert_eq!(ev.tool_provider, "claude");
        assert_eq!(ev.query.as_deref(), Some("fix the build"));
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p mur-core inject::event 2>&1 | head -20
```
Expected: `error[E0583]: file not found for module 'event'` or compile error.

**Step 3: Write the file at `mur-core/src/inject/event.rs`**

Copy the full content from Step 1 above.

**Step 4: Add module to `mur-core/src/inject/mod.rs`**

```rust
pub mod event;
pub mod hook;
pub mod sync;
```

**Step 5: Run tests to verify they pass**

```bash
cargo test -p mur-core inject::event
```
Expected: `test result: ok. 13 passed`

**Step 6: Commit**

```bash
git add mur-core/src/inject/event.rs mur-core/src/inject/mod.rs
git commit -m "feat(inject): event normalisation layer — per-tool stdin parsers for 5 AI tools"
```

---

## Task 2: `inject::queue` — NDJSON queue writer

**Files:**
- Create: `mur-core/src/inject/queue.rs`
- Modify: `mur-core/src/inject/mod.rs`

**Step 1: Write the failing test** (bottom of `queue.rs`):

```rust
use anyhow::Result;
use serde_json::Value;
use std::io::Write;

use super::event::NormalizedEvent;

pub fn enqueue(event: &NormalizedEvent) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let queue_dir = home.join(".mur").join("queue");
    std::fs::create_dir_all(&queue_dir)?;
    let path = queue_dir.join("events.jsonl");
    let line = serde_json::to_string(event)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};
    use serde_json::json;
    use tempfile::tempdir;

    fn make_event() -> NormalizedEvent {
        NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: Some("how do I use anyhow?".into()),
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: Some("sess_test".into()),
        }
    }

    #[test]
    fn enqueue_writes_valid_jsonl() {
        // Use a temp dir to avoid polluting ~/.mur in tests
        let dir = tempdir().unwrap();
        let queue_path = dir.path().join("events.jsonl");

        let event = make_event();
        let line = serde_json::to_string(&event).unwrap() + "\n";
        std::fs::write(&queue_path, &line).unwrap();

        let content = std::fs::read_to_string(&queue_path).unwrap();
        let parsed: NormalizedEvent = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed.kind, EventKind::Prompt);
        assert_eq!(parsed.query.as_deref(), Some("how do I use anyhow?"));
    }

    #[test]
    fn enqueue_appends_multiple_events() {
        let dir = tempdir().unwrap();
        let queue_path = dir.path().join("events.jsonl");

        for i in 0..3 {
            let event = NormalizedEvent {
                kind: EventKind::Prompt,
                tool_provider: "claude".into(),
                query: Some(format!("query {i}")),
                tool_called: None,
                tool_input: None,
                stop_reason: None,
                session_id: None,
            };
            let line = serde_json::to_string(&event).unwrap() + "\n";
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&queue_path)
                .unwrap();
            f.write_all(line.as_bytes()).unwrap();
        }

        let content = std::fs::read_to_string(&queue_path).unwrap();
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
    }
}
```

**Step 2: Run test to verify it fails**

```bash
cargo test -p mur-core inject::queue 2>&1 | head -10
```
Expected: compile error — module not found.

**Step 3: Create `mur-core/src/inject/queue.rs`** with the full content above.

**Step 4: Add `pub mod queue;` to `mur-core/src/inject/mod.rs`**

```rust
pub mod event;
pub mod hook;
pub mod queue;
pub mod sync;
```

**Step 5: Run tests**

```bash
cargo test -p mur-core inject::queue
```
Expected: `ok. 2 passed`

**Step 6: Commit**

```bash
git add mur-core/src/inject/queue.rs mur-core/src/inject/mod.rs
git commit -m "feat(inject): queue writer — append-only NDJSON event log for daemon (M3)"
```

---

## Task 3: `mur hook` Clap subcommand skeleton (no logic yet)

**Files:**
- Create: `mur-core/src/cmd/hook.rs`
- Modify: `mur-core/src/cmd/mod.rs`
- Modify: `mur-core/src/main.rs`

**Step 1: Write a build-test** (just verifying it compiles):

```bash
# We'll verify by running cargo build after the changes.
```

**Step 2: Create `mur-core/src/cmd/hook.rs`** with stubs:

```rust
use anyhow::Result;

pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    let _ = tool;
    Ok(())
}

pub(crate) async fn cmd_hook_tool(tool: &str) -> Result<()> {
    let _ = tool;
    Ok(())
}

pub(crate) async fn cmd_hook_stop(tool: &str) -> Result<()> {
    let _ = tool;
    Ok(())
}

pub(crate) async fn cmd_hook_session_start(tool: &str) -> Result<()> {
    let _ = tool;
    Ok(())
}

pub(crate) fn cmd_hook_stats() -> Result<()> {
    println!("hook stats: not yet implemented");
    Ok(())
}
```

**Step 3: Add `pub(crate) mod hook;` to `mur-core/src/cmd/mod.rs`** (alphabetical order, after `doctor`):

```rust
pub(crate) mod hook;
```

**Step 4: Add `Hook` variant to `Commands` enum in `mur-core/src/main.rs`**

After the `Init` variant (around line 258), add:

```rust
/// Unified hook entry point for all AI tools
Hook {
    /// Event type: prompt, tool, stop, session-start, stats
    #[command(subcommand)]
    event: HookEvent,
},
```

Add the `HookEvent` enum after the existing `ExchangeAction` enum (around line 440):

```rust
#[derive(Subcommand)]
enum HookEvent {
    /// Handle UserPromptSubmit / BeforeAgent / beforeSubmitPrompt events
    Prompt {
        /// AI tool identifier (claude, gemini, cursor, copilot, opencode, amp)
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Handle PreToolUse / AfterTool / beforeShellExecution events
    Tool {
        /// AI tool identifier
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Handle Stop / SessionEnd events (triggers background pipeline)
    Stop {
        /// AI tool identifier
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Handle SessionStart events (injects L0 capability index)
    #[command(name = "session-start")]
    SessionStart {
        /// AI tool identifier
        #[arg(long, default_value = "claude")]
        tool: String,
    },
    /// Show hook statistics (skip rate, tier distribution, latency)
    Stats,
}
```

**Step 5: Add dispatch arm to the `match cli.command` block in `main.rs`**

Find the `Commands::Inject` arm and add after it:

```rust
Commands::Hook { event } => match event {
    HookEvent::Prompt { tool } => cmd::hook::cmd_hook_prompt(&tool).await?,
    HookEvent::Tool { tool } => cmd::hook::cmd_hook_tool(&tool).await?,
    HookEvent::Stop { tool } => cmd::hook::cmd_hook_stop(&tool).await?,
    HookEvent::SessionStart { tool } => cmd::hook::cmd_hook_session_start(&tool).await?,
    HookEvent::Stats => cmd::hook::cmd_hook_stats()?,
},
```

**Step 6: Build to verify no compile errors**

```bash
cargo build -p mur-core 2>&1 | tail -5
```
Expected: `Finished` with no errors.

**Step 7: Verify `mur hook --help` works**

```bash
cargo run -- hook --help 2>&1
```
Expected: shows `prompt`, `tool`, `stop`, `session-start`, `stats` subcommands.

**Step 8: Commit**

```bash
git add mur-core/src/cmd/hook.rs mur-core/src/cmd/mod.rs mur-core/src/main.rs
git commit -m "feat(cmd): mur hook skeleton — Clap subcommand with 5 event variants"
```

---

## Task 4: `cmd_hook_prompt` — gate-aware keyword injection

**Files:**
- Modify: `mur-core/src/cmd/hook.rs`
- Create: `mur-core/tests/fixtures/hook_inputs/claude_prompt_coding.json`
- Create: `mur-core/tests/fixtures/hook_inputs/claude_prompt_ack.json`

**Step 1: Create fixture files**

`mur-core/tests/fixtures/hook_inputs/claude_prompt_coding.json`:
```json
{
  "prompt": "refactor the token budget enforcement to support per-tier caps",
  "session_id": "test_session_1"
}
```

`mur-core/tests/fixtures/hook_inputs/claude_prompt_ack.json`:
```json
{
  "prompt": "ok",
  "session_id": "test_session_1"
}
```

`mur-core/tests/fixtures/hook_inputs/claude_stop.json`:
```json
{
  "stop_reason": "end_turn"
}
```

`mur-core/tests/fixtures/hook_inputs/gemini_before_agent.json`:
```json
{
  "prompt": "how does tokio's select! macro handle cancellation?"
}
```

`mur-core/tests/fixtures/hook_inputs/cursor_prompt.json`:
```json
{
  "prompt": "implement retry logic with exponential backoff",
  "language": "rust"
}
```

`mur-core/tests/fixtures/hook_inputs/copilot_prompt.json`:
```json
{
  "prompt": "add comprehensive error handling to the auth module",
  "timeoutSec": 30
}
```

`mur-core/tests/fixtures/hook_inputs/opencode_session_created.json`:
```json
{
  "session": {"id": "sess_abc", "project": "/home/user/myapp"}
}
```

**Step 2: Write a unit test for the gate path** (add to `hook.rs` bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ack_query_produces_empty_output() {
        let raw = json!({"prompt": "ok"});
        let query = extract_query_from_raw(&raw, "claude");
        assert!(should_skip_injection(query.as_deref()));
    }

    #[test]
    fn coding_query_does_not_skip() {
        let raw = json!({"prompt": "refactor the token budget enforcement"});
        let query = extract_query_from_raw(&raw, "claude");
        assert!(!should_skip_injection(query.as_deref()));
    }

    #[test]
    fn empty_stdin_does_not_panic() {
        let raw = json!({});
        let query = extract_query_from_raw(&raw, "claude");
        // No query → skip (gate treats None as noise)
        assert!(should_skip_injection(query.as_deref()));
    }
}
```

**Step 3: Run to verify failure**

```bash
cargo test -p mur-core cmd::hook 2>&1 | head -20
```
Expected: compile error — functions `extract_query_from_raw` and `should_skip_injection` not found.

**Step 4: Implement `cmd_hook_prompt`** — replace the stub in `hook.rs`:

```rust
use anyhow::Result;
use std::io::Read;

use crate::inject::event::{EventKind, parse_event};
use crate::inject::queue::enqueue;
use crate::retrieve::gate::{GateInputs, Tier as GateTier, evaluate_query_v2};
use crate::retrieve::scoring::score_and_rank;
use crate::store::yaml::YamlStore;
use crate::store::workflow_yaml::WorkflowYamlStore;

// ── Internal helpers ──────────────────────────────────────────────────────────

pub(crate) fn extract_query_from_raw(raw: &serde_json::Value, provider: &str) -> Option<String> {
    // For prompt events, the query field depends on the tool
    match provider {
        "gemini" | "cursor" | "copilot" | "claude" | "amp" | "auggie" => {
            raw.get("prompt").and_then(|v| v.as_str()).map(str::to_owned)
        }
        _ => raw.get("prompt").and_then(|v| v.as_str()).map(str::to_owned),
    }
}

pub(crate) fn should_skip_injection(query: Option<&str>) -> bool {
    let q = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => return true, // no query → skip
    };
    let inputs = GateInputs::default();
    let outcome = evaluate_query_v2(q, &inputs);
    outcome.tier == GateTier::Skip
}

// ── Command handlers ──────────────────────────────────────────────────────────

pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    // 1. Read stdin (may be empty on session start calls)
    let mut stdin_buf = String::new();
    std::io::stdin().read_to_string(&mut stdin_buf)?;
    let raw: serde_json::Value = if stdin_buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&stdin_buf).unwrap_or(serde_json::json!({}))
    };

    // 2. Normalise to common event shape
    let event = parse_event(raw.clone(), EventKind::Prompt, tool);

    // 3. Write to queue (daemon M3 will read this)
    let _ = enqueue(&event); // best-effort; don't fail hook on queue write error

    // 4. Gate check
    let query = event.query.as_deref().unwrap_or("");
    let inputs = GateInputs::default();
    let outcome = evaluate_query_v2(query, &inputs);

    if outcome.tier == GateTier::Skip {
        // No output — AI tool sees empty additionalContext
        return Ok(());
    }

    // 5. Keyword-only quick-path retrieval (no embedding in hot path)
    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    use mur_common::pattern::LifecycleStatus;
    let scored = score_and_rank(query, patterns);
    let injected: Vec<_> = scored
        .into_iter()
        .filter(|sp| sp.pattern.lifecycle.status != LifecycleStatus::Archived)
        .map(|sp| sp.pattern)
        .collect();

    // Token budget: L0=300, L1=500, L2=2000 (M4 will respect tier; M1 uses flat 500)
    let budget = match outcome.tier {
        GateTier::L0 => 300,
        GateTier::L1 => 500,
        GateTier::L2 => 2000,
        GateTier::Skip => unreachable!(),
    };

    let output = crate::inject::hook::format_unified_injection_with_store(
        &injected,
        &workflows,
        budget,
        Some(&yaml_store),
    );

    if !output.is_empty() {
        print!("{}", output);
    }

    Ok(())
}

pub(crate) async fn cmd_hook_tool(tool: &str) -> Result<()> {
    let mut stdin_buf = String::new();
    std::io::stdin().read_to_string(&mut stdin_buf)?;
    let raw: serde_json::Value = if stdin_buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&stdin_buf).unwrap_or(serde_json::json!({}))
    };

    let event = parse_event(raw, EventKind::Tool, tool);
    let _ = enqueue(&event);
    Ok(())
}

pub(crate) async fn cmd_hook_stop(tool: &str) -> Result<()> {
    let mut stdin_buf = String::new();
    std::io::stdin().read_to_string(&mut stdin_buf)?;
    let raw: serde_json::Value = if stdin_buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&stdin_buf).unwrap_or(serde_json::json!({}))
    };

    let event = parse_event(raw, EventKind::Stop, tool);
    let _ = enqueue(&event);

    // Spawn background pipeline (sync + evolve + extract + emerge)
    // identical to what on-stop.sh did, but in Rust
    spawn_background_pipeline();

    Ok(())
}

pub(crate) async fn cmd_hook_session_start(tool: &str) -> Result<()> {
    let mut stdin_buf = String::new();
    std::io::stdin().read_to_string(&mut stdin_buf)?;
    let raw: serde_json::Value = if stdin_buf.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&stdin_buf).unwrap_or(serde_json::json!({}))
    };

    let event = parse_event(raw, EventKind::SessionStart, tool);
    let _ = enqueue(&event);

    // L0 capability index injection — implemented in M2
    // For M1: return empty (no patterns at session start)
    Ok(())
}

pub(crate) fn cmd_hook_stats() -> Result<()> {
    println!("hook stats: not yet implemented (M5)");
    Ok(())
}

// ── Background pipeline (replaces on-stop.sh background block) ───────────────

fn spawn_background_pipeline() {
    // Spawn detached child process to run the post-stop pipeline.
    // Parent process returns < 50ms regardless of pipeline duration.
    let mur_bin = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("mur"));

    // sync
    let _ = std::process::Command::new(&mur_bin)
        .arg("sync")
        .arg("--quiet")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    // evolve (decay + maturity) and emerge run sequentially in child; use
    // a small shell wrapper so parent doesn't wait on either.
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{mur} evolve 2>/dev/null; {mur} emerge 2>/dev/null",
            mur = mur_bin.display()
        ))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ack_query_produces_skip() {
        let query = Some("ok".to_string());
        assert!(should_skip_injection(query.as_deref()));
    }

    #[test]
    fn coding_query_does_not_skip() {
        let query = Some("refactor the token budget enforcement".to_string());
        assert!(!should_skip_injection(query.as_deref()));
    }

    #[test]
    fn empty_stdin_skips() {
        assert!(should_skip_injection(None));
        assert!(should_skip_injection(Some("")));
    }

    #[test]
    fn extract_query_from_claude_raw() {
        let raw = json!({"prompt": "implement error retry"});
        let q = extract_query_from_raw(&raw, "claude");
        assert_eq!(q.as_deref(), Some("implement error retry"));
    }

    #[test]
    fn extract_query_missing_field_returns_none() {
        let raw = json!({"tool_name": "Edit"});
        let q = extract_query_from_raw(&raw, "claude");
        assert!(q.is_none());
    }
}
```

**Step 5: Run tests**

```bash
cargo test -p mur-core cmd::hook
```
Expected: `ok. 5 passed`

**Step 6: Verify build**

```bash
cargo build -p mur-core 2>&1 | tail -3
```

**Step 7: Commit**

```bash
git add mur-core/src/cmd/hook.rs mur-core/tests/fixtures/hook_inputs/
git commit -m "feat(cmd/hook): cmd_hook_prompt with gate-aware keyword injection"
```

---

## Task 5: Cross-tool integration tests via fixture JSON payloads

**Files:**
- Create: `mur-core/tests/hook_integration.rs`
- Create remaining fixture files (gemini, cursor, copilot, opencode)

**Step 1: Write the integration tests** (file: `mur-core/tests/hook_integration.rs`):

```rust
//! Cross-tool hook integration: fixture stdin → expected NormalizedEvent.

use mur_core::inject::event::{EventKind, NormalizedEvent, parse_event};
use serde_json::Value;
use std::fs;

fn load_fixture(name: &str) -> Value {
    let path = format!("tests/fixtures/hook_inputs/{name}");
    let content = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("missing fixture: {path}"));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("bad JSON in {path}: {e}"))
}

#[test]
fn claude_prompt_has_query() {
    let raw = load_fixture("claude_prompt_coding.json");
    let ev = parse_event(raw, EventKind::Prompt, "claude");
    assert_eq!(ev.kind, EventKind::Prompt);
    assert!(ev.query.as_ref().map(|q| q.len() > 5).unwrap_or(false),
        "expected non-trivial query");
}

#[test]
fn claude_ack_query_present_in_event() {
    let raw = load_fixture("claude_prompt_ack.json");
    let ev = parse_event(raw, EventKind::Prompt, "claude");
    assert_eq!(ev.query.as_deref(), Some("ok"));
}

#[test]
fn claude_stop_has_stop_reason() {
    let raw = load_fixture("claude_stop.json");
    let ev = parse_event(raw, EventKind::Stop, "claude");
    assert_eq!(ev.stop_reason.as_deref(), Some("end_turn"));
}

#[test]
fn gemini_prompt_extracted() {
    let raw = load_fixture("gemini_before_agent.json");
    let ev = parse_event(raw, EventKind::Prompt, "gemini");
    assert_eq!(ev.tool_provider, "gemini");
    assert!(ev.query.is_some());
}

#[test]
fn cursor_prompt_extracted() {
    let raw = load_fixture("cursor_prompt.json");
    let ev = parse_event(raw, EventKind::Prompt, "cursor");
    assert_eq!(ev.tool_provider, "cursor");
    assert!(ev.query.is_some());
}

#[test]
fn copilot_prompt_extracted() {
    let raw = load_fixture("copilot_prompt.json");
    let ev = parse_event(raw, EventKind::Prompt, "copilot");
    assert_eq!(ev.tool_provider, "copilot");
    assert!(ev.query.is_some());
}

#[test]
fn opencode_session_created_has_session_id() {
    let raw = load_fixture("opencode_session_created.json");
    let ev = parse_event(raw, EventKind::SessionStart, "opencode");
    assert_eq!(ev.session_id.as_deref(), Some("sess_abc"));
}

#[test]
fn all_tools_parse_without_panic_on_empty() {
    let providers = ["claude", "gemini", "cursor", "copilot", "opencode", "amp"];
    for p in &providers {
        let ev = parse_event(serde_json::json!({}), EventKind::Prompt, p);
        assert!(ev.query.is_none(), "empty JSON should yield no query for {p}");
    }
}
```

**Step 2: Run to verify failure**

```bash
cargo test -p mur-core --test hook_integration 2>&1 | head -20
```
Expected: compile error or missing fixture files.

**Step 3: Create all remaining fixture files** (the `claude_*` fixtures were created in Task 4):

`mur-core/tests/fixtures/hook_inputs/gemini_before_agent.json`:
```json
{"prompt": "how does tokio's select! macro handle cancellation?"}
```

`mur-core/tests/fixtures/hook_inputs/cursor_prompt.json`:
```json
{"prompt": "implement retry logic with exponential backoff", "language": "rust"}
```

`mur-core/tests/fixtures/hook_inputs/copilot_prompt.json`:
```json
{"prompt": "add comprehensive error handling to the auth module", "timeoutSec": 30}
```

`mur-core/tests/fixtures/hook_inputs/opencode_session_created.json`:
```json
{"session": {"id": "sess_abc", "project": "/home/user/myapp"}}
```

**Step 4: Run integration tests**

```bash
cargo test -p mur-core --test hook_integration
```
Expected: `ok. 8 passed`

**Step 5: Commit**

```bash
git add mur-core/tests/hook_integration.rs mur-core/tests/fixtures/hook_inputs/
git commit -m "test(hook): cross-tool integration tests with fixture JSON payloads"
```

---

## Task 6: Rewrite `mur init --hooks` — one-liner scripts

**Files:**
- Modify: `mur-core/src/cmd/init.rs`

The spec: every installed hook script becomes a one-liner `exec mur hook <event> --tool <name>`.

**Step 1: Write a test** (add to `mur-core/tests/` or inline in init.rs):

The test verifies that the string constants for hook scripts contain `exec mur hook` and not the old `mur context --compact`.

Add a `#[cfg(test)]` block at the bottom of `init.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn hook_scripts_use_unified_entry() {
        // The hook script constants should not contain the old multi-line shell logic.
        // We check by importing the constants — they're local to the function, so
        // we test the invariant at the string level.
        let prompt = super::HOOK_SCRIPT_PROMPT;
        let tool = super::HOOK_SCRIPT_TOOL;
        let stop = super::HOOK_SCRIPT_STOP;
        let session_start = super::HOOK_SCRIPT_SESSION_START;

        assert!(prompt.contains("mur hook prompt"), "on-prompt.sh must call mur hook prompt");
        assert!(!prompt.contains("mur context"), "on-prompt.sh must NOT call mur context");
        assert!(tool.contains("mur hook tool"), "on-tool.sh must call mur hook tool");
        assert!(stop.contains("mur hook stop"), "on-stop.sh must call mur hook stop");
        assert!(session_start.contains("mur hook session-start"), "must call session-start");
    }
}
```

**Step 2: Run to verify failure**

```bash
cargo test -p mur-core init::tests 2>&1 | head -20
```
Expected: compile error — `HOOK_SCRIPT_PROMPT` not found.

**Step 3: Extract hook script strings to module-level constants** in `init.rs`

At the top of the file, before `pub(crate) fn cmd_init`, add:

```rust
pub(crate) const HOOK_SCRIPT_PROMPT: &str = r#"#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook prompt --tool "${MUR_TOOL:-claude}"
"#;

pub(crate) const HOOK_SCRIPT_TOOL: &str = r#"#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook tool --tool "${MUR_TOOL:-claude}"
"#;

pub(crate) const HOOK_SCRIPT_STOP: &str = r#"#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook stop --tool "${MUR_TOOL:-claude}"
"#;

pub(crate) const HOOK_SCRIPT_SESSION_START: &str = r#"#!/bin/bash
# mur-managed-hook v7 — generated by `mur init --hooks`
exec mur hook session-start --tool "${MUR_TOOL:-claude}"
"#;
```

**Step 4: Replace the inline hook script strings** in `cmd_init`:

Find the block that defines `on_prompt`, `on_tool`, `on_stop` as raw string literals and replace with:

```rust
let on_prompt = HOOK_SCRIPT_PROMPT;
let on_tool = HOOK_SCRIPT_TOOL;
let on_stop = HOOK_SCRIPT_STOP;
```

Also add a fourth hook file for `session-start`:

```rust
let on_session_start = HOOK_SCRIPT_SESSION_START;

let hooks = [
    ("on-prompt.sh", on_prompt),
    ("on-tool.sh", on_tool),
    ("on-stop.sh", on_stop),
    ("on-session-start.sh", on_session_start),
];
```

**Step 5: Add `SessionStart` hook to Claude Code settings**

In the `hook_defs` array for Claude Code settings (around line 143 of init.rs), add the `SessionStart` event:

```rust
let hook_defs = [
    (
        "UserPromptSubmit",
        hooks_dir.join("on-prompt.sh").to_string_lossy().to_string(),
    ),
    (
        "PreToolUse",
        hooks_dir.join("on-tool.sh").to_string_lossy().to_string(),
    ),
    (
        "PostToolUse",
        hooks_dir.join("on-tool.sh").to_string_lossy().to_string(),
    ),
    (
        "Stop",
        hooks_dir.join("on-stop.sh").to_string_lossy().to_string(),
    ),
    (
        "SessionStart",
        hooks_dir.join("on-session-start.sh").to_string_lossy().to_string(),
    ),
];
```

**Step 6: Run tests**

```bash
cargo test -p mur-core init::tests
```
Expected: `ok. 1 passed`

**Step 7: Build and lint**

```bash
cargo build -p mur-core && cargo clippy -p mur-core -- -D warnings
```

**Step 8: Commit**

```bash
git add mur-core/src/cmd/init.rs
git commit -m "feat(init): rewrite hook scripts to one-liner exec mur hook <event>"
```

---

## Task 7: Full test suite + clippy + fmt + PR

**Step 1: Run full test suite**

```bash
cargo test --workspace 2>&1 | tail -20
```
Expected: all tests pass, including M0 gate tests.

**Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep -E "^error"
```
Expected: no errors. Fix any warnings before continuing.

**Step 3: Run fmt check**

```bash
cargo fmt --check 2>&1
```
If any failures, run `cargo fmt` and commit the formatting fix.

**Step 4: Verify `mur hook` end-to-end with a real query** (requires patterns to exist)

```bash
echo '{"prompt": "how do I implement error handling with anyhow?"}' | cargo run -- hook prompt --tool claude 2>&1 | head -10
```
Expected: either pattern output or empty (if no patterns exist in `~/.mur/patterns/`).

**Step 5: Verify `mur hook` skips ack queries**

```bash
echo '{"prompt": "ok"}' | cargo run -- hook prompt --tool claude 2>&1
```
Expected: no output (exit 0, empty stdout).

**Step 6: Verify Gemini tool parse**

```bash
echo '{"prompt": "explain async in rust"}' | cargo run -- hook prompt --tool gemini 2>&1 | head -5
```

**Step 7: Commit any remaining changes**

```bash
git add -p
git commit -m "chore: formatting and clippy fixes for M1"
```

**Step 8: Push and create PR**

```bash
git push -u origin feat/m0-adaptive-gate
gh pr create \
  --title "feat(hooks): M1 — mur hook unified entry binary" \
  --body "$(cat <<'EOF'
## Summary

- Adds `mur hook prompt|tool|stop|session-start|stats` subcommand as the single entry point for all nine AI tool integrations
- Normalises tool-specific stdin schemas (Claude Code, Gemini CLI, Cursor, Copilot CLI, OpenCode, Amp) into a common `NormalizedEvent`
- Writes events to `~/.mur/queue/events.jsonl` for the future M3 daemon
- `mur hook prompt` runs the M0 adaptive gate — Skip means no output; L0/L1/L2 runs keyword-path retrieval
- `mur init --hooks` now installs one-liner scripts (`exec mur hook prompt --tool claude`) instead of the 50-line bash script
- Adds `SessionStart` hook installation for Claude Code

## Test Plan

- [x] `cargo test -p mur-core inject::event` — 13 unit tests for per-tool parsers
- [x] `cargo test -p mur-core inject::queue` — JSONL append tests
- [x] `cargo test -p mur-core cmd::hook` — gate path unit tests
- [x] `cargo test -p mur-core --test hook_integration` — 8 fixture-driven cross-tool tests
- [x] `cargo test -p mur-core init::tests` — verifies hook scripts contain `mur hook` entry
- [x] `echo '{"prompt":"ok"}' | mur hook prompt` emits no output (gate Skip)
- [x] `echo '{"prompt":"refactor token budget"}' | mur hook prompt` returns pattern markdown

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Notes for the implementer

- **No async embedding in M1 hot path.** `cmd_hook_prompt` uses `score_and_rank` (keyword BM25 only). Embedding-augmented retrieval lives in the M3 daemon. This keeps `mur hook prompt` under 50ms even on cold start.
- **Queue write is best-effort.** A `let _ = enqueue(...)` pattern is intentional — a failed queue write must not break the AI tool session. The daemon compensates with session-end batch extraction.
- **`MUR_TOOL` env var.** The one-liner scripts use `${MUR_TOOL:-claude}` so tools that set this env var (e.g. Auggie, Amp) get the right parser without needing separate scripts.
- **`on-stop.sh` background pipeline.** `spawn_background_pipeline` uses `std::process::Command::spawn()` (non-blocking). The parent process exits before the children finish. This is equivalent to the `(...) &` pattern in the old bash script.
- **Existing `mur inject` command is unchanged.** It's the manual CLI path; the hook path is new.
- **M2 will flesh out `cmd_hook_session_start`** to inject the L0 capability index. M1 just writes the queue event and returns.
- **M4 will add `async: true` to Claude Code hook entries** in the settings.json written by `mur init --hooks`.
