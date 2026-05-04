# M0 — Adaptive Gate Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use `superpowers:executing-plans` to implement this plan task-by-task.

**Goal:** Replace the 3-state `Skip / Force / Pass` gate with a 4-tier scored gate (`Skip / L0 / L1 / L2`) that combines intent regex, recent tool-call history, query quality, and session state — eliminating the bug where greetings ("ok", "符合", "thanks") trigger full pattern injection.

**Architecture:** A pure local classifier (no LLM, no vector lookup, < 5 ms total) at `mur-core/src/retrieve/gate.rs`. Composite score `0.30·intent + 0.25·tool_signal + 0.20·query_quality + 0.15·session_state + 0.10·prefetch_hit` (prefetch_hit deferred to M3 — fixed at 0.0 for now). Returns a `GateOutcome { tier: Tier, score: f32, reasons: Vec<&str> }`. All four current call sites (`cmd/inject_cmd.rs`, `cmd/pattern.rs`, `context_api/mod.rs`, internal tests) migrate at once — pre-launch, no back-compat per `project_pre_launch_no_backcompat.md`.

**Tech Stack:** Rust 2024, `regex`, `serde_json` (for reading `~/.mur/session/active.json`), existing `mur_common::event::SessionEvent`, existing `crate::capture::noise_filter`.

**Effect of M0 alone:** Greetings/ack stop triggering injection immediately. Token spend on routine turns drops from ~2000 to 0. L1/L2 differentiation is wired but content-side still uses today's `format_unified_injection_with_store` (M2/M4 add real per-tier formatters).

---

## Pre-flight

Run once before Task 1:

```bash
git checkout main
git status                       # working tree clean
cargo build --workspace          # baseline must build
cargo test -p mur-core retrieve::gate     # baseline must pass
```

If any of those fail, stop and surface to user. Do not start in a dirty tree.

---

### Task 1: Define `Tier` enum and `GateOutcome` struct

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs:1-71` (replace existing public types)

**Step 1: Write the failing test**

Append to `mur-core/src/retrieve/gate.rs` test module (after line 217):

```rust
    #[test]
    fn test_tier_ordering() {
        // Tier should support ordering: Skip < L0 < L1 < L2
        assert!(Tier::Skip < Tier::L0);
        assert!(Tier::L0 < Tier::L1);
        assert!(Tier::L1 < Tier::L2);
    }

    #[test]
    fn test_outcome_construction() {
        let o = GateOutcome { tier: Tier::L1, score: 0.62, reasons: vec!["intent: action verb"] };
        assert_eq!(o.tier, Tier::L1);
        assert!((o.score - 0.62).abs() < 1e-6);
        assert_eq!(o.reasons.len(), 1);
    }
```

**Step 2: Run test, verify it fails**

```bash
cargo test -p mur-core retrieve::gate::tests::test_tier_ordering
```
Expected: `error[E0433]: failed to resolve: use of undeclared type \`Tier\``

**Step 3: Replace the `GateDecision` enum with `Tier` + `GateOutcome`**

Replace `mur-core/src/retrieve/gate.rs:6-14`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// 0 tokens — pure ack / greeting / noise
    Skip,
    /// Capability index only (~150-300 tokens, SessionStart layer)
    L0,
    /// 1-3 pattern snippets (~500 tokens)
    L1,
    /// Full body + linked workflows (~1500-2000 tokens)
    L2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateOutcome {
    pub tier: Tier,
    pub score: f32,
    pub reasons: Vec<&'static str>,
}
```

**Step 4: Run tests, verify pass**

```bash
cargo build -p mur-core 2>&1 | head -30
```
Expected: many compile errors at call sites (`cmd/inject_cmd.rs:11`, `cmd/pattern.rs:193`, `context_api/mod.rs:14`, gate test module). Suppress them temporarily by leaving `evaluate_query` returning a stub `GateOutcome`:

```rust
pub fn evaluate_query(query: &str) -> GateOutcome {
    let _ = query;
    GateOutcome { tier: Tier::L1, score: 0.5, reasons: vec![] }
}
```

Delete the entire body of `evaluate_query` (lines 47-71) and the `GateDecision` references in callers:

- `cmd/inject_cmd.rs:11-31`: replace the `if let GateDecision::Skip(reason) = ...` block with `if matches!(crate::retrieve::gate::evaluate_query(query).tier, Tier::Skip) { return Ok(()); }` (import `Tier` from `crate::retrieve::gate`).
- `cmd/pattern.rs:193-205`: same swap pattern; the `Force` arm becomes `Tier::L2`, `Pass` becomes `Tier::L1 | Tier::L0`.
- `context_api/mod.rs:140`: same swap.

Delete all old gate tests at `gate.rs:73-217` that reference `GateDecision`. They will be replaced in later tasks.

```bash
cargo build -p mur-core
cargo test -p mur-core retrieve::gate
```
Expected: builds clean. The 2 new tests from Step 1 pass.

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs mur-core/src/cmd/inject_cmd.rs mur-core/src/cmd/pattern.rs mur-core/src/context_api/mod.rs
git commit -m "refactor(gate): replace GateDecision with 4-tier Tier + GateOutcome"
```

---

### Task 2: Intent score (regex + length)

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs` (add private `intent_score` fn)

**Step 1: Write failing tests**

Append to test module:

```rust
    use super::intent_score;

    #[test]
    fn intent_pure_ack_zero() {
        assert_eq!(intent_score("ok"), 0.0);
        assert_eq!(intent_score("好"), 0.0);
        assert_eq!(intent_score("thanks"), 0.0);
        assert_eq!(intent_score("符合"), 0.0);
        assert_eq!(intent_score("OK!"), 0.0);
    }

    #[test]
    fn intent_meta_command_zero() {
        assert_eq!(intent_score("/help"), 0.0);
        assert_eq!(intent_score("/status"), 0.0);
        assert_eq!(intent_score("/model gpt-4"), 0.0);
    }

    #[test]
    fn intent_question_low() {
        assert!((intent_score("為什麼會這樣") - 0.3).abs() < 1e-6);
        assert!((intent_score("what is RAG") - 0.3).abs() < 1e-6);
    }

    #[test]
    fn intent_code_identifier_mid() {
        assert!((intent_score("look at mod.rs") - 0.7).abs() < 1e-6);
        assert!((intent_score("the fn handle_event() is broken") - 0.7).abs() < 1e-6);
    }

    #[test]
    fn intent_action_verb_high() {
        assert!((intent_score("實作 adaptive gate") - 0.8).abs() < 1e-6);
        assert!((intent_score("refactor the auth module") - 0.8).abs() < 1e-6);
        assert!((intent_score("fix the build error") - 0.8).abs() < 1e-6);
    }

    #[test]
    fn intent_long_technical_max() {
        let q = "I want to add a new tokio worker that subscribes to events.jsonl and runs LLM extraction in the background pool";
        assert!((intent_score(q) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn intent_fallback_mid() {
        assert!((intent_score("can you help me with this thing") - 0.5).abs() < 1e-6);
    }
```

**Step 2: Run tests, verify they fail**

```bash
cargo test -p mur-core retrieve::gate::tests::intent_
```
Expected: `cannot find function \`intent_score\` in this scope`

**Step 3: Implement `intent_score`**

Add after the `GateOutcome` struct in `gate.rs`:

```rust
static ACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(ok|okay|好|好的|thanks|thank you|thx|sure|yes|no|nope|對|不對|是|嗯|沒問題|了解|收到|got it|understood|符合|fine|cool|nice|great)[\s\.!\?]*$").unwrap()
});

static META_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^/(help|status|model|clear|usage|exit|quit|effort|fast|review|init|config|mcp|cost|memory|hooks?|permissions?|agents?|todos?)\b").unwrap()
});

static QUESTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(why|what is|what's|how does|explain|為什麼|是什麼|解釋一下|怎麼回事)\b").unwrap()
});

static CODE_IDENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    // file paths with extensions, fn calls, snake_case identifiers, ::paths
    Regex::new(r"(\w+\.[a-zA-Z]{1,5}\b|\bfn\s+\w+|\w+::\w+|\b[a-z]+_[a-z_]+\b)").unwrap()
});

static ACTION_VERB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(實作|實現|修|改|加|建立|刪除|測試|refactor|implement|build|fix|add|remove|delete|create|test|debug|deploy|migrate|rewrite|integrate|wire|hook|extract)\b").unwrap()
});

const TECH_TERMS: &[&str] = &[
    "tokio", "async", "await", "spawn", "select", "trait", "struct", "enum", "impl",
    "test", "build", "debug", "deploy", "lint", "format", "refactor", "migrate",
    "error", "panic", "result", "option", "vec", "hashmap", "btreemap",
    "api", "endpoint", "request", "response", "header", "json", "yaml",
    "database", "schema", "table", "column", "index", "query", "transaction",
    "worker", "queue", "daemon", "thread", "lock", "channel", "future",
    "tcp", "http", "https", "ssl", "tls", "noise", "websocket",
    "docker", "kubernetes", "ci", "cd", "pipeline", "hook", "skill", "agent",
    "vector", "embedding", "retrieval", "rag", "llm", "prompt", "pattern",
];

fn count_tech_terms(query_lower: &str) -> usize {
    TECH_TERMS.iter().filter(|t| query_lower.contains(*t)).count()
}

pub(crate) fn intent_score(query: &str) -> f32 {
    let trimmed = query.trim();
    if ACK_RE.is_match(trimmed) {
        return 0.0;
    }
    if META_RE.is_match(trimmed) {
        return 0.0;
    }
    if QUESTION_RE.is_match(trimmed) {
        return 0.3;
    }

    let lower = trimmed.to_lowercase();
    let tech_count = count_tech_terms(&lower);
    let char_count = trimmed.chars().count();

    if char_count > 80 && tech_count >= 2 {
        return 0.9;
    }
    if ACTION_VERB_RE.is_match(trimmed) {
        return 0.8;
    }
    if CODE_IDENT_RE.is_match(trimmed) {
        return 0.7;
    }
    0.5
}
```

**Step 4: Run tests, verify they pass**

```bash
cargo test -p mur-core retrieve::gate::tests::intent_
```
Expected: 7 passed.

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs
git commit -m "feat(gate): intent_score — regex + length + tech term composite"
```

---

### Task 3: Tool signal score (read recent tool calls)

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs` (add `tool_signal_score`)

**Step 1: Write failing tests**

Append:

```rust
    use super::{tool_signal_score, ToolSignalInput};

    fn ts(tool: &str, cmd: Option<&str>) -> ToolSignalInput {
        ToolSignalInput { tool: tool.into(), bash_command: cmd.map(String::from) }
    }

    #[test]
    fn tool_signal_no_history_low() {
        assert!((tool_signal_score(&[]) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_only_read_only() {
        let h = vec![ts("Read", None), ts("Glob", None), ts("Grep", None)];
        assert!((tool_signal_score(&h) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_edit_present_high() {
        let h = vec![ts("Read", None), ts("Edit", None)];
        assert!((tool_signal_score(&h) - 0.9).abs() < 1e-6);
        let h2 = vec![ts("Write", None)];
        assert!((tool_signal_score(&h2) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_build_command_high() {
        let h = vec![ts("Bash", Some("cargo test --workspace"))];
        assert!((tool_signal_score(&h) - 0.8).abs() < 1e-6);
        let h2 = vec![ts("Bash", Some("npm run build"))];
        assert!((tool_signal_score(&h2) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_mcp_mid() {
        let h = vec![ts("mcp__chrome-devtools__navigate_page", None)];
        assert!((tool_signal_score(&h) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn tool_signal_edit_wins_over_read() {
        // Edit/Write must win even when Read is also present
        let h = vec![ts("Read", None), ts("Bash", Some("ls")), ts("Edit", None)];
        assert!((tool_signal_score(&h) - 0.9).abs() < 1e-6);
    }
```

**Step 2: Run tests, verify they fail**

```bash
cargo test -p mur-core retrieve::gate::tests::tool_signal_
```
Expected: undeclared `tool_signal_score`.

**Step 3: Implement**

Add to `gate.rs`:

```rust
#[derive(Debug, Clone)]
pub struct ToolSignalInput {
    pub tool: String,
    pub bash_command: Option<String>,
}

const BUILD_TEST_RUNNERS: &[&str] = &[
    "cargo", "npm", "yarn", "pnpm", "bun", "pytest", "go", "make", "docker",
    "rustc", "gcc", "clang", "mvn", "gradle", "swift", "xcodebuild",
];

pub(crate) fn tool_signal_score(history: &[ToolSignalInput]) -> f32 {
    if history.is_empty() {
        return 0.1;
    }

    let mut has_edit = false;
    let mut has_build = false;
    let mut has_mcp = false;
    let mut only_read = true;

    for entry in history {
        match entry.tool.as_str() {
            "Edit" | "Write" | "NotebookEdit" => {
                has_edit = true;
                only_read = false;
            }
            "Read" | "Glob" | "Grep" => {}
            "Bash" => {
                only_read = false;
                if let Some(cmd) = &entry.bash_command {
                    let first_word = cmd.split_whitespace().next().unwrap_or("");
                    if BUILD_TEST_RUNNERS.iter().any(|r| first_word == *r) {
                        has_build = true;
                    }
                }
            }
            t if t.starts_with("mcp__") => {
                has_mcp = true;
                only_read = false;
            }
            _ => {
                only_read = false;
            }
        }
    }

    if has_edit { return 0.9; }
    if has_build { return 0.8; }
    if has_mcp { return 0.7; }
    if only_read { return 0.4; }
    0.5
}
```

**Step 4: Run tests, verify pass**

```bash
cargo test -p mur-core retrieve::gate::tests::tool_signal_
```
Expected: 6 passed.

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs
git commit -m "feat(gate): tool_signal_score from recent tool history"
```

---

### Task 4: Read tool history from `~/.mur/session/active.json`

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs` (add `read_recent_tool_history`)

**Step 1: Write failing test**

Append:

```rust
    use super::read_recent_tool_history;
    use std::io::Write as _;

    #[test]
    fn read_history_from_recordings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("session/recordings");
        std::fs::create_dir_all(&session_dir).unwrap();

        // active.json points to a session id; recordings/<id>.jsonl holds events
        let active = serde_json::json!({"session_id": "test-sess"});
        std::fs::write(tmp.path().join("session/active.json"),
            serde_json::to_string(&active).unwrap()).unwrap();

        let mut f = std::fs::File::create(session_dir.join("test-sess.jsonl")).unwrap();
        // 3 oldest first
        writeln!(f, r#"{{"event_type":"tool_call","tool":"Read","content":"{{}}"}}"#).unwrap();
        writeln!(f, r#"{{"event_type":"tool_call","tool":"Bash","content":"{{\"command\":\"cargo test\"}}"}}"#).unwrap();
        writeln!(f, r#"{{"event_type":"tool_call","tool":"Edit","content":"{{}}"}}"#).unwrap();

        let h = read_recent_tool_history(tmp.path(), 5);
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].tool, "Edit");           // most recent last
        assert_eq!(h[1].tool, "Bash");
        assert_eq!(h[1].bash_command.as_deref(), Some("cargo test"));
    }

    #[test]
    fn read_history_missing_active_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let h = read_recent_tool_history(tmp.path(), 5);
        assert!(h.is_empty());
    }
```

Add this dev-dep if not present (check first):

```bash
grep -A 10 '\[dev-dependencies\]' mur-core/Cargo.toml
```
If `tempfile` is missing, add it to `mur-core/Cargo.toml` under `[dev-dependencies]`:

```toml
tempfile = "3"
```

**Step 2: Run tests, verify they fail**

```bash
cargo test -p mur-core retrieve::gate::tests::read_history_
```
Expected: undeclared `read_recent_tool_history`.

**Step 3: Implement**

Add to `gate.rs` (use `std::path::Path` and import `serde_json`):

```rust
use std::path::Path;

/// Read the last `n` tool calls from the active session recording.
///
/// Returns oldest-first order so callers can use the slice in sequence.
/// Returns empty Vec on any error (no active session, malformed events,
/// missing files) — gate degrades gracefully to "no history" signal.
pub(crate) fn read_recent_tool_history(mur_dir: &Path, n: usize) -> Vec<ToolSignalInput> {
    let active_path = mur_dir.join("session/active.json");
    let Ok(active_raw) = std::fs::read_to_string(&active_path) else {
        return Vec::new();
    };
    let Ok(active_json): Result<serde_json::Value, _> = serde_json::from_str(&active_raw) else {
        return Vec::new();
    };
    let Some(session_id) = active_json.get("session_id").and_then(|v| v.as_str()) else {
        return Vec::new();
    };

    let recording = mur_dir.join("session/recordings").join(format!("{session_id}.jsonl"));
    let Ok(content) = std::fs::read_to_string(&recording) else {
        return Vec::new();
    };

    let mut tool_events: Vec<ToolSignalInput> = content
        .lines()
        .filter_map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).ok()?;
            if v.get("event_type")?.as_str()? != "tool_call" {
                return None;
            }
            let tool = v.get("tool")?.as_str()?.to_string();
            let bash_command = v
                .get("content")
                .and_then(|c| c.as_str())
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|cv| cv.get("command")?.as_str().map(String::from));
            Some(ToolSignalInput { tool, bash_command })
        })
        .collect();

    let total = tool_events.len();
    if total > n {
        tool_events.drain(..total - n);
    }
    tool_events
}
```

**Step 4: Run tests, verify pass**

```bash
cargo test -p mur-core retrieve::gate::tests::read_history_
```
Expected: 2 passed.

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs mur-core/Cargo.toml
git commit -m "feat(gate): read_recent_tool_history from session recordings"
```

---

### Task 5: Query quality wrapper

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs`

**Step 1: Write failing tests**

```rust
    use super::query_quality_score;

    #[test]
    fn quality_pass_full() {
        assert!((query_quality_score("Refactor the gate module to support tier scoring") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn quality_noise_zero() {
        assert!((query_quality_score("ok") - 0.0).abs() < 1e-6);
        assert!((query_quality_score("👍") - 0.0).abs() < 1e-6);
        assert!((query_quality_score("") - 0.0).abs() < 1e-6);
    }
```

**Step 2: Run, verify fail**

```bash
cargo test -p mur-core retrieve::gate::tests::quality_
```

**Step 3: Implement**

```rust
use crate::capture::noise_filter::{filter, FilterResult};

pub(crate) fn query_quality_score(query: &str) -> f32 {
    match filter(query) {
        FilterResult::Pass => 1.0,
        FilterResult::Noise(_) => 0.0,
    }
}
```

**Step 4: Verify pass**

```bash
cargo test -p mur-core retrieve::gate::tests::quality_
```

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs
git commit -m "feat(gate): query_quality_score wrapping noise_filter"
```

---

### Task 6: Session state score

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs`

**Step 1: Write failing tests**

```rust
    use super::{session_state_score, SessionStateInput};
    use chrono::{Duration, Utc};

    #[test]
    fn session_fresh_high() {
        let s = SessionStateInput {
            age: Duration::seconds(10),
            seconds_since_last_edit: None,
        };
        assert!((session_state_score(&s) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn session_active_edit_max() {
        let s = SessionStateInput {
            age: Duration::minutes(5),
            seconds_since_last_edit: Some(30),
        };
        assert!((session_state_score(&s) - 0.9).abs() < 1e-6);
    }

    #[test]
    fn session_idle_low() {
        let s = SessionStateInput {
            age: Duration::minutes(45),
            seconds_since_last_edit: None,
        };
        assert!((session_state_score(&s) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn session_default_mid() {
        let s = SessionStateInput {
            age: Duration::minutes(5),
            seconds_since_last_edit: Some(600),
        };
        assert!((session_state_score(&s) - 0.5).abs() < 1e-6);
    }
```

**Step 2: Run, verify fail**

```bash
cargo test -p mur-core retrieve::gate::tests::session_
```

**Step 3: Implement**

```rust
use chrono::Duration;

#[derive(Debug, Clone)]
pub struct SessionStateInput {
    pub age: Duration,
    pub seconds_since_last_edit: Option<i64>,
}

pub(crate) fn session_state_score(input: &SessionStateInput) -> f32 {
    if let Some(s) = input.seconds_since_last_edit {
        if s < 60 {
            return 0.9;
        }
    }
    if input.age < Duration::seconds(30) {
        return 0.7;
    }
    if input.age > Duration::minutes(30) && input.seconds_since_last_edit.is_none() {
        return 0.3;
    }
    0.5
}
```

**Step 4: Verify pass**

```bash
cargo test -p mur-core retrieve::gate::tests::session_
```

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs
git commit -m "feat(gate): session_state_score (age + recent-edit recency)"
```

---

### Task 7: Composite `evaluate_query` and tier mapping

**Files:**
- Modify: `mur-core/src/retrieve/gate.rs` (rewrite `evaluate_query`)

**Step 1: Write failing tests**

```rust
    use super::{evaluate_query_v2, GateInputs};

    fn empty_inputs() -> GateInputs {
        GateInputs {
            tool_history: Vec::new(),
            session_state: SessionStateInput { age: Duration::minutes(5), seconds_since_last_edit: None },
        }
    }

    #[test]
    fn end_to_end_skip_on_ack() {
        let o = evaluate_query_v2("ok", &empty_inputs());
        assert_eq!(o.tier, Tier::Skip);
    }

    #[test]
    fn end_to_end_skip_on_符合() {
        let o = evaluate_query_v2("符合", &empty_inputs());
        assert_eq!(o.tier, Tier::Skip);
    }

    #[test]
    fn end_to_end_l0_on_short_question() {
        let o = evaluate_query_v2("what is RAG", &empty_inputs());
        assert!(matches!(o.tier, Tier::L0 | Tier::L1));
    }

    #[test]
    fn end_to_end_l2_on_coding_with_edit_history() {
        let mut i = empty_inputs();
        i.tool_history = vec![
            ToolSignalInput { tool: "Read".into(), bash_command: None },
            ToolSignalInput { tool: "Edit".into(), bash_command: None },
        ];
        i.session_state.seconds_since_last_edit = Some(20);
        let o = evaluate_query_v2(
            "implement the adaptive gate composite scoring with tokio worker pool",
            &i,
        );
        assert_eq!(o.tier, Tier::L2);
    }

    #[test]
    fn end_to_end_workflow_keyword_forces_l1() {
        let o = evaluate_query_v2("agent-browser去pchome-24h找airpods-pro的價格", &empty_inputs());
        // workflow trigger detection bypasses score; M0 baseline ≥ L1 if length OK
        assert!(o.tier >= Tier::L1);
    }
```

**Step 2: Run, verify fail**

```bash
cargo test -p mur-core retrieve::gate::tests::end_to_end_
```

**Step 3: Implement composite**

Replace the stub `evaluate_query` body and add `evaluate_query_v2` plus a public alias:

```rust
#[derive(Debug, Clone)]
pub struct GateInputs {
    pub tool_history: Vec<ToolSignalInput>,
    pub session_state: SessionStateInput,
}

impl Default for GateInputs {
    fn default() -> Self {
        Self {
            tool_history: Vec::new(),
            session_state: SessionStateInput {
                age: Duration::seconds(0),
                seconds_since_last_edit: None,
            },
        }
    }
}

pub fn evaluate_query_v2(query: &str, inputs: &GateInputs) -> GateOutcome {
    let intent = intent_score(query);
    let tool = tool_signal_score(&inputs.tool_history);
    let quality = query_quality_score(query);
    let session = session_state_score(&inputs.session_state);
    let prefetch = 0.0_f32;  // M3 will fill this in

    let score = 0.30 * intent + 0.25 * tool + 0.20 * quality + 0.15 * session + 0.10 * prefetch;

    let tier = if score < 0.30 { Tier::Skip }
        else if score < 0.50 { Tier::L0 }
        else if score < 0.80 { Tier::L1 }
        else { Tier::L2 };

    let mut reasons = Vec::new();
    if intent == 0.0 { reasons.push("intent: noise/ack/meta"); }
    if quality == 0.0 { reasons.push("quality: noise filter rejected"); }
    if tool >= 0.8 { reasons.push("tool: edit/build active"); }

    GateOutcome { tier, score, reasons }
}

/// Convenience: evaluate using only the query text. Reads tool history from disk.
/// Returns Skip if mur-dir cannot be located.
pub fn evaluate_query(query: &str) -> GateOutcome {
    let Some(home) = dirs::home_dir() else {
        return GateOutcome { tier: Tier::Skip, score: 0.0, reasons: vec!["no home dir"] };
    };
    let mur_dir = home.join(".mur");
    let inputs = GateInputs {
        tool_history: read_recent_tool_history(&mur_dir, 5),
        session_state: SessionStateInput {
            age: Duration::seconds(0),
            seconds_since_last_edit: None,
        },
    };
    evaluate_query_v2(query, &inputs)
}
```

**Step 4: Verify pass**

```bash
cargo test -p mur-core retrieve::gate
```
Expected: all 5 tests in this task pass plus prior 18 from earlier tasks.

**Step 5: Commit**

```bash
git add mur-core/src/retrieve/gate.rs
git commit -m "feat(gate): composite evaluate_query_v2 with 4-tier scoring"
```

---

### Task 8: Wire callers to honor tier semantics

**Files:**
- Modify: `mur-core/src/cmd/inject_cmd.rs:9-31`
- Modify: `mur-core/src/cmd/pattern.rs:193-205`
- Modify: `mur-core/src/context_api/mod.rs:140`

**Step 1: Write failing test (integration)**

Create new test file `mur-core/tests/gate_integration.rs`:

```rust
//! Integration: ack-style queries must skip injection entirely.

use mur_core::retrieve::gate::{evaluate_query_v2, GateInputs, Tier};

#[test]
fn ack_short_words_skip_in_default_inputs() {
    let inputs = GateInputs::default();
    for q in &["ok", "好", "thanks", "符合", "OK!", "嗯", "對"] {
        let o = evaluate_query_v2(q, &inputs);
        assert_eq!(o.tier, Tier::Skip, "query {:?} should skip, got {:?}", q, o);
    }
}

#[test]
fn meta_commands_skip() {
    let inputs = GateInputs::default();
    for q in &["/help", "/status", "/model gpt-4", "/clear"] {
        let o = evaluate_query_v2(q, &inputs);
        assert_eq!(o.tier, Tier::Skip, "query {:?} should skip", q);
    }
}
```

Note: requires exposing `gate` module publicly — verify `mur-core/src/retrieve/mod.rs` already has `pub mod gate;`. If not, fix it.

```bash
grep "pub mod gate" mur-core/src/retrieve/mod.rs
```

If missing, change `mod gate;` to `pub mod gate;` and add `pub use gate::{evaluate_query, evaluate_query_v2, GateInputs, GateOutcome, Tier};` re-export.

**Step 2: Run, verify fail**

```bash
cargo test -p mur-core --test gate_integration
```
Expected: compile fail (pub access) or assertion fail.

**Step 3: Update call sites**

In `mur-core/src/cmd/inject_cmd.rs`, replace lines 11-31 with:

```rust
    use crate::retrieve::gate::{evaluate_query, Tier};
    use crate::retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::LanceDbStore as VectorStore;
    use std::collections::HashMap;

    let outcome = evaluate_query(query);
    if outcome.tier == Tier::Skip {
        eprintln!("# No patterns (gate: skip, score={:.2}, reasons={:?})", outcome.score, outcome.reasons);
        return Ok(());
    }
```

Drop the `detect_trigger`/`HookTrigger` import block from this file — it is no longer used. (Trigger printing is decorative; M4 will reintroduce per-tier behavior. For M0 we just ensure Skip exits and L0/L1/L2 fall through to today's retrieval.)

In `mur-core/src/cmd/pattern.rs:193-205`, replace with:

```rust
    use crate::retrieve::gate::{evaluate_query, Tier};
    let outcome = evaluate_query(query);
    if outcome.tier == Tier::Skip {
        println!("# Gate skipped: {} (score={:.2})", outcome.reasons.join(", "), outcome.score);
        return Ok(());
    }
```

In `mur-core/src/context_api/mod.rs:140`:

```rust
    use crate::retrieve::gate::{evaluate_query, Tier};
    if evaluate_query(&req.query).tier == Tier::Skip {
        return Ok(empty_response());  // existing helper or build inline
    }
```

(Inspect the file first; preserve whatever empty-response shape it currently uses.)

**Step 4: Run all tests**

```bash
cargo test -p mur-core
cargo test -p mur-core --test gate_integration
cargo build --workspace
```
Expected: all pass.

**Step 5: Commit**

```bash
git add mur-core/src/cmd/inject_cmd.rs mur-core/src/cmd/pattern.rs mur-core/src/context_api/mod.rs mur-core/src/retrieve/mod.rs mur-core/tests/gate_integration.rs
git commit -m "refactor(gate): wire callers to Tier::Skip short-circuit"
```

---

### Task 9: 100-query golden set

**Files:**
- Create: `mur-core/tests/fixtures/gate_golden_set.jsonl`
- Create: `mur-core/tests/gate_golden_set.rs`

**Step 1: Write golden fixtures**

Create `mur-core/tests/fixtures/gate_golden_set.jsonl` with **100 lines**, JSON per line:

```jsonl
{"query": "ok", "expected_tier": "Skip"}
{"query": "好", "expected_tier": "Skip"}
{"query": "thanks", "expected_tier": "Skip"}
{"query": "符合", "expected_tier": "Skip"}
{"query": "嗯", "expected_tier": "Skip"}
{"query": "/help", "expected_tier": "Skip"}
{"query": "/status", "expected_tier": "Skip"}
{"query": "👍", "expected_tier": "Skip"}
{"query": "🎉🚀", "expected_tier": "Skip"}
{"query": "no", "expected_tier": "Skip"}
... (continue to 100)
```

**Distribution target**:
- 30 should-skip (greetings/ack/meta/single emoji/<3 chars)
- 20 should-pass-as-L0 (short questions, "what is X")
- 30 should-pass-as-L1 (medium queries, action verbs, file refs)
- 20 should-pass-as-L2 (long technical queries with multiple tech terms)

Mix Chinese and English roughly 40/60. Source examples from real past prompts (`mur-core/tests/fixtures/` may already have transcripts; check first), augment with synthetic.

```bash
ls mur-core/tests/fixtures/ 2>/dev/null
ls mur-core/src/cmd/__fixtures__/ 2>/dev/null
```

**Step 2: Write the runner test**

Create `mur-core/tests/gate_golden_set.rs`:

```rust
//! Golden set accuracy test: ≥ 85% must hit the expected tier.

use mur_core::retrieve::gate::{evaluate_query_v2, GateInputs, Tier};
use serde::Deserialize;

#[derive(Deserialize)]
struct Row {
    query: String,
    expected_tier: String,
}

fn parse_tier(s: &str) -> Tier {
    match s {
        "Skip" => Tier::Skip,
        "L0" => Tier::L0,
        "L1" => Tier::L1,
        "L2" => Tier::L2,
        other => panic!("unknown tier in golden set: {other}"),
    }
}

#[test]
fn golden_set_accuracy_at_least_85_percent() {
    let raw = std::fs::read_to_string("tests/fixtures/gate_golden_set.jsonl")
        .expect("missing fixture");
    let rows: Vec<Row> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad row {l}: {e}")))
        .collect();

    assert_eq!(rows.len(), 100, "fixture must contain 100 rows");

    let inputs = GateInputs::default();
    let mut hits = 0;
    let mut misses: Vec<(String, Tier, Tier)> = Vec::new();

    for row in &rows {
        let expected = parse_tier(&row.expected_tier);
        let actual = evaluate_query_v2(&row.query, &inputs).tier;
        if actual == expected {
            hits += 1;
        } else {
            misses.push((row.query.clone(), expected, actual));
        }
    }

    let accuracy = hits as f32 / rows.len() as f32;
    if accuracy < 0.85 {
        for (q, want, got) in &misses {
            eprintln!("MISS: {q:?} want={want:?} got={got:?}");
        }
        panic!("golden set accuracy {:.2} < 0.85 ({} hits / {} total)", accuracy, hits, rows.len());
    }
}

#[test]
fn golden_set_skip_recall_perfect() {
    // Stricter sub-test: every "Skip" row MUST be classified as Skip
    let raw = std::fs::read_to_string("tests/fixtures/gate_golden_set.jsonl").unwrap();
    let rows: Vec<Row> = raw.lines().filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap()).collect();

    let inputs = GateInputs::default();
    for row in &rows {
        if row.expected_tier == "Skip" {
            let actual = evaluate_query_v2(&row.query, &inputs).tier;
            assert_eq!(actual, Tier::Skip, "row {:?} must skip but got {:?}", row.query, actual);
        }
    }
}
```

If `serde` is not already in `mur-core/Cargo.toml` `[dev-dependencies]`, add:

```toml
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

(Most likely already present — check first.)

**Step 3: Run, verify fail**

```bash
cargo test -p mur-core --test gate_golden_set
```
Expected: FAIL — fixture missing, or accuracy below 0.85, or some Skip rows fail.

**Step 4: Iterate fixture + regex tuning**

Run the test repeatedly. For each `MISS:` line:

- If a Skip row comes back as L0/L1: add the missed phrase to `ACK_RE` or `META_RE` in `gate.rs`.
- If an L1 row comes back as L0: lower the `0.50` boundary or strengthen `ACTION_VERB_RE` / `CODE_IDENT_RE`.
- If an L2 row comes back as L1: ensure `count_tech_terms` covers the relevant terms.

**Critically: the golden set is the spec, not the regex.** Tune classifier regex/weights toward the fixture, not the other way around. If you cannot reach 85% without contorting the regex, surface the conflict to the user — do not weaken assertions.

Re-run until both tests pass:

```bash
cargo test -p mur-core --test gate_golden_set
```
Expected: both tests PASS, accuracy printout absent (only printed on failure).

**Step 5: Commit**

```bash
git add mur-core/tests/fixtures/gate_golden_set.jsonl mur-core/tests/gate_golden_set.rs mur-core/Cargo.toml mur-core/src/retrieve/gate.rs
git commit -m "test(gate): 100-query golden set, ≥85% accuracy + perfect skip recall"
```

---

### Task 10: Manual smoke test against current bug

**Files:** none — exercise the binary

**Step 1: Build + install dev binary**

```bash
cargo build --release -p mur-core
```

**Step 2: Run the buggy queries and verify they no longer inject**

```bash
target/release/mur inject "ok"
target/release/mur inject "符合"
target/release/mur inject "thanks"
target/release/mur inject "好"
```

Expected output for each:

```
# No patterns (gate: skip, score=0.00, reasons=["intent: noise/ack/meta", "quality: noise filter rejected"])
```

**Step 3: Run a real query, verify it still injects**

```bash
target/release/mur inject "implement adaptive gate composite scoring with tokio worker pool"
```

Expected: returns retrieved patterns with the existing `## Relevant knowledge from your learning history` header (M0 keeps current formatting; M2/M4 differentiate).

**Step 4: Verify hook script behavior**

```bash
echo '{"prompt": "ok"}' | bash ~/.mur/hooks/on-prompt.sh
```

Expected: no pattern dump (the script calls `mur context --compact` internally; that path also routes through `evaluate_query`).

**Step 5: Run full test suite, lint, format**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```
Expected: green across the board.

---

## Verification checklist before marking M0 complete

Run through `superpowers:verification-before-completion`. Required outputs:

- [ ] `cargo test --workspace` passes (paste full pass count)
- [ ] `cargo test -p mur-core --test gate_golden_set` passes (paste both test names)
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] Manual smoke (Task 10 Steps 2-3) shows skip on "ok" / "符合" / "thanks" and pass-through on the long technical query
- [ ] All commits present (Tasks 1-10 plus golden-set tuning increments)
- [ ] No `GateDecision` references remain: `git grep GateDecision mur-core/` returns nothing

If any item fails, do NOT claim M0 done — fix the underlying issue first.

---

## Out of scope for M0 (deferred to later milestones)

- Per-tier injection content differentiation (L0 vs L1 vs L2 produce same body today) → **M2 / M4**
- `prefetch_hit` signal (set to 0.0) → **M3**
- `mur hook` unified entry binary → **M1**
- New `mur init --hooks` shell scripts using `exec mur hook` → **M1**
- `murmurd` daemon → **M3**
- Telemetry / `mur hook stats` → **M5**

M0 explicitly only changes the gate's decision logic and short-circuits Skip cases. The visible bug (greetings injecting 7 patterns) is fixed because all four call sites bail at `Tier::Skip` before reaching retrieval.
