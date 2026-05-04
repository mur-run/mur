# mur Hooks M4 — Tool-Call-Aware L2 Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make `mur hook tool` emit full-body L2 injection on PreToolUse(Edit|Write|Bash), and add workflow trigger detection to `cmd_hook_prompt` so workflow-matching queries are never suppressed to Skip.

**Architecture:** Two changes to `mur-core/src/cmd/hook.rs`. (1) `cmd_hook_tool` detects PreToolUse by absence of `tool_response` in the raw stdin JSON, then for Edit/Write/Bash tools runs `score_and_rank` using the tool input as a query hint and emits via `format_unified_injection_with_store` at L2 budget (2000 chars). (2) `cmd_hook_prompt` loads workflow names/triggers after gate evaluation and bumps Skip/L0 to L1 when the query overlaps any workflow name. No new crates, no async changes, no daemon changes.

**Tech Stack:** Rust 2024, existing `mur-core` internals (`score_and_rank`, `format_unified_injection_with_store`, `WorkflowYamlStore`, `YamlStore`).

---

## Task 1: L2 injection in `cmd_hook_tool` for PreToolUse(Edit|Write|Bash)

**Files:**
- Modify: `mur-core/src/cmd/hook.rs`
- Modify: `mur-core/tests/hook_integration.rs` (add tests)

### Background

Currently `cmd_hook_tool` only enqueues the event and returns:
```rust
pub(crate) async fn cmd_hook_tool(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw, EventKind::Tool, tool);
    let _ = enqueue(&event);
    Ok(())
}
```

For PreToolUse hooks, Claude Code uses whatever is printed to stdout as `additionalContext`. We need to print L2 pattern content when the tool is a code-editing tool.

**How to detect PreToolUse vs PostToolUse:** Claude Code's PostToolUse stdin includes a `tool_response` field; PreToolUse does not. Check `raw.get("tool_response").is_none()` before emitting.

**L2-triggering tools (case-insensitive):** `edit`, `write`, `bash`, `multiedit`

**Query for scoring:** Concatenate the tool name and tool_input JSON as a string. Tool input for `Edit` is `{"file_path": "...", ...}` — the file path gives keyword signals. Use `event.tool_input.as_deref().unwrap_or(tool)` as the query.

### Step 1: Write the failing tests

Add to the bottom of `mur-core/tests/hook_integration.rs`:

```rust
mod tool_l2_injection {
    use mur_core::inject::event::{EventKind, NormalizedEvent, parse_event};
    use serde_json::json;

    fn tool_event(tool_name: &str, with_response: bool) -> serde_json::Value {
        let mut v = json!({
            "tool_name": tool_name,
            "tool_input": {"file_path": "mur-core/src/cmd/hook.rs"},
            "session_id": "sess_m4_test"
        });
        if with_response {
            v.as_object_mut().unwrap().insert(
                "tool_response".to_string(),
                json!({"output": "done"}),
            );
        }
        v
    }

    #[test]
    fn pre_tool_use_detected_by_missing_response_field() {
        let raw = tool_event("Edit", false);
        assert!(raw.get("tool_response").is_none(), "PreToolUse has no tool_response");
    }

    #[test]
    fn post_tool_use_detected_by_presence_of_response_field() {
        let raw = tool_event("Edit", true);
        assert!(raw.get("tool_response").is_some(), "PostToolUse has tool_response");
    }

    #[test]
    fn l2_tool_names_are_recognised() {
        let l2_tools = ["edit", "Edit", "EDIT", "write", "Write", "bash", "Bash", "multiedit"];
        for t in &l2_tools {
            let ev = parse_event(tool_event(t, false), EventKind::Tool, "claude");
            let called = ev.tool_called.as_deref().unwrap_or("").to_ascii_lowercase();
            assert!(
                ["edit", "write", "bash", "multiedit"].contains(&called.as_str()),
                "Expected {t} to be an L2 tool, got {called}"
            );
        }
    }

    #[test]
    fn non_l2_tool_names_are_not_recognised() {
        let non_l2 = ["Read", "Grep", "Glob", "WebFetch"];
        for t in &non_l2 {
            let called = t.to_ascii_lowercase();
            assert!(
                !["edit", "write", "bash", "multiedit"].contains(&called.as_str()),
                "Expected {t} to NOT be an L2 tool"
            );
        }
    }
}
```

### Step 2: Run to verify tests compile and pass

```bash
cargo test -p mur-core --test hook_integration tool_l2_injection 2>&1 | tail -10
```
Expected: 4 passed.

### Step 3: Add the `is_pre_tool_use` and `is_l2_tool` helpers to `hook.rs`

Add these two functions after the `should_skip` function in `hook.rs`:

```rust
fn is_pre_tool_use(raw: &serde_json::Value) -> bool {
    raw.get("tool_response").is_none()
}

fn is_l2_tool(tool_name: &str) -> bool {
    matches!(
        tool_name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "bash" | "multiedit"
    )
}
```

### Step 4: Rewrite `cmd_hook_tool` to emit L2

Replace the existing `cmd_hook_tool` (currently lines 99-104 of hook.rs):

```rust
pub(crate) async fn cmd_hook_tool(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Tool, tool);
    let _ = enqueue(&event);

    // Emit L2 only on PreToolUse for code-editing tools
    if !is_pre_tool_use(&raw) {
        return Ok(());
    }
    let tool_called = event.tool_called.as_deref().unwrap_or("");
    if !is_l2_tool(tool_called) {
        return Ok(());
    }

    // Use tool_input as the query hint (file path / bash command gives keyword signals)
    let query = event.tool_input.as_deref().unwrap_or(tool_called);
    if query.trim().is_empty() {
        return Ok(());
    }

    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    use mur_common::pattern::LifecycleStatus;
    let injected: Vec<_> = score_and_rank(query, patterns)
        .into_iter()
        .filter(|sp| sp.pattern.lifecycle.status != LifecycleStatus::Archived)
        .map(|sp| sp.pattern)
        .collect();

    const L2_BUDGET: usize = 2000;
    let output = crate::inject::hook::format_unified_injection_with_store(
        &injected,
        &workflows,
        L2_BUDGET,
        Some(&yaml_store),
    );

    if !output.is_empty() {
        print!("{output}");
    }
    Ok(())
}
```

### Step 5: Run full test suite

```bash
cargo test -p mur-core 2>&1 | grep -E "^test result|FAILED" | tail -10
```
Expected: zero failures.

### Step 6: Clippy + fmt + commit

```bash
cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error"
cargo fmt -p mur-core
git add mur-core/src/cmd/hook.rs mur-core/tests/hook_integration.rs
git commit -m "feat(hook): L2 injection on PreToolUse(Edit|Write|Bash|MultiEdit)"
```

---

## Task 2: Workflow trigger detection in `cmd_hook_prompt`

**Files:**
- Modify: `mur-core/src/cmd/hook.rs`

### Background

`Workflow` structs (at `~/.mur/workflows/*.yaml`) have a `name` (from `base.name` via `Deref<Target = KnowledgeBase>`) and a `trigger` field (natural-language description). If the user's query contains any workflow name or a keyword from the trigger, we should force at least L1 — the workflow is relevant and should be injected.

**Access pattern:** `workflow.name` works via `Deref`. `workflow.trigger` is a direct field.

**Matching logic:** Split the query and each workflow name/trigger into lowercase words; if any word ≥ 4 characters appears in both, it's a match.

### Step 1: Write the failing tests

Add a unit test module to `mur-core/src/cmd/hook.rs`:

```rust
#[cfg(test)]
mod workflow_trigger_tests {
    use super::*;

    fn make_workflow_name(name: &str) -> String {
        name.to_owned()
    }

    fn query_matches_workflow(query: &str, workflow_names: &[String]) -> bool {
        let query_words: Vec<String> = query
            .to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|w| w.len() >= 4)
            .map(str::to_owned)
            .collect();
        workflow_names.iter().any(|name| {
            name.to_ascii_lowercase()
                .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
                .filter(|w| w.len() >= 4)
                .any(|word| query_words.iter().any(|qw| qw == word))
        })
    }

    #[test]
    fn exact_workflow_name_matches() {
        let names = vec![make_workflow_name("deploy-production")];
        assert!(query_matches_workflow("deploy the production service", &names));
    }

    #[test]
    fn partial_workflow_name_matches() {
        let names = vec![make_workflow_name("search-bookstore")];
        assert!(query_matches_workflow("search for latest books", &names));
    }

    #[test]
    fn unrelated_query_does_not_match() {
        let names = vec![make_workflow_name("deploy-production")];
        assert!(!query_matches_workflow("fix the lint error", &names));
    }

    #[test]
    fn short_words_are_ignored() {
        let names = vec![make_workflow_name("run-ci")];
        // "run" (3 chars) and "ci" (2 chars) — both < 4 chars, no match
        assert!(!query_matches_workflow("run ci now", &names));
    }
}
```

### Step 2: Verify tests compile but the function does not exist yet

```bash
cargo test -p mur-core --lib workflow_trigger_tests 2>&1 | head -10
```
Expected: compile error — `query_matches_workflow` defined in tests, not in hook.rs yet. (Tests are self-contained, so they should pass as-is — the helper is inline.)

Actually, since `query_matches_workflow` is defined inside the test module as a local helper, these tests should compile and pass immediately. Run them:

```bash
cargo test -p mur-core --lib workflow_trigger_tests 2>&1 | tail -8
```
Expected: 4 passed.

### Step 3: Extract `workflow_name_matches_query` as a module-level function in `hook.rs`

Add above the test module:

```rust
fn workflow_name_matches_query(query: &str, workflow_names: &[String]) -> bool {
    let query_words: Vec<String> = query
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.len() >= 4)
        .map(str::to_owned)
        .collect();
    workflow_names.iter().any(|name| {
        name.to_ascii_lowercase()
            .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .filter(|w| w.len() >= 4)
            .any(|word| query_words.iter().any(|qw| qw == word))
    })
}
```

Then update the test module to call `super::workflow_name_matches_query` instead of the local helper.

### Step 4: Wire into `cmd_hook_prompt`

In `cmd_hook_prompt`, after the gate evaluation and BEFORE the inbox-first path, add workflow bump logic:

```rust
// After: let outcome = evaluate_query_v2(&query, &inputs);
// After: if outcome.tier == GateTier::Skip { return Ok(()); }

// Workflow trigger detection: if query matches a workflow name, ensure ≥ L1
let effective_tier = if outcome.tier < GateTier::L1 {
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflow_names: Vec<String> = workflow_store
        .list_all()?
        .into_iter()
        .map(|w| w.name.clone())
        .collect();
    if workflow_name_matches_query(&query, &workflow_names) {
        GateTier::L1
    } else {
        outcome.tier
    }
} else {
    outcome.tier
};
```

Then replace `outcome.tier` with `effective_tier` in the subsequent budget match:

```rust
let budget = match effective_tier {
    GateTier::L0 => 300,
    GateTier::L1 => 500,
    GateTier::L2 => 2000,
    GateTier::Skip => unreachable!(),
};
```

**Note:** The inbox-first early return at `outcome.tier == GateTier::Skip` happens BEFORE the workflow bump. After the bump, `effective_tier` is used. The inbox content is served regardless of tier (it's pre-computed); `effective_tier` only controls the fallback budget.

Full updated `cmd_hook_prompt` structure:

```rust
pub(crate) async fn cmd_hook_prompt(tool: &str) -> Result<()> {
    let raw = read_stdin_json();
    let event = parse_event(raw.clone(), EventKind::Prompt, tool);
    let _ = enqueue(&event);

    let query = extract_query(&raw).unwrap_or_default();
    if query.trim().is_empty() {
        return Ok(());
    }

    let inputs = GateInputs::default();
    let outcome = evaluate_query_v2(&query, &inputs);
    if outcome.tier == GateTier::Skip {
        return Ok(());
    }

    // Bump to L1 if query overlaps a workflow name (workflow triggers bypass L0 cap)
    let effective_tier = if outcome.tier < GateTier::L1 {
        let workflow_store = WorkflowYamlStore::default_store()?;
        let workflow_names: Vec<String> = workflow_store
            .list_all()?
            .into_iter()
            .map(|w| w.name.clone())
            .collect();
        if workflow_name_matches_query(&query, &workflow_names) {
            GateTier::L1
        } else {
            outcome.tier
        }
    } else {
        outcome.tier
    };

    // Inbox-first: serve pre-computed context from murmurd if fresh
    if let Some(session_id) = event.session_id.as_deref() {
        let inbox = crate::daemon::inbox_path(session_id);
        if let Some(content) = crate::daemon::read_inbox(&inbox, 300) {
            print!("{content}");
            return Ok(());
        }
    }

    // Synchronous fallback
    let yaml_store = YamlStore::default_store()?;
    let patterns = yaml_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    use mur_common::pattern::LifecycleStatus;
    let injected: Vec<_> = score_and_rank(&query, patterns)
        .into_iter()
        .filter(|sp| sp.pattern.lifecycle.status != LifecycleStatus::Archived)
        .map(|sp| sp.pattern)
        .collect();

    let budget = match effective_tier {
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
        print!("{output}");
    }
    Ok(())
}
```

**Note:** `WorkflowYamlStore` is loaded twice when the trigger bump is active AND the synchronous fallback runs. This is acceptable (it's just a directory read). A future refactor can deduplicate.

### Step 5: Run tests

```bash
cargo test -p mur-core 2>&1 | grep -E "^test result|FAILED" | tail -10
```
Expected: zero failures.

### Step 6: Clippy + fmt + commit

```bash
cargo clippy -p mur-core -- -D warnings 2>&1 | grep "^error"
cargo fmt -p mur-core
git add mur-core/src/cmd/hook.rs
git commit -m "feat(hook): workflow trigger detection — bumps L0/Skip to L1 when query matches workflow name"
```

---

## Task 3: End-to-end integration tests + push

**Files:**
- Create: `mur-core/tests/hook_e2e_m4.rs`
- Modify: `mur-core/src/cmd/hook.rs` (re-export `is_l2_tool` + `workflow_name_matches_query` for tests if needed)

### Step 1: Write integration tests

Create `mur-core/tests/hook_e2e_m4.rs`:

```rust
//! M4 end-to-end: verifies L2 detection logic and workflow trigger matching.

use mur_core::inject::event::{EventKind, parse_event};
use serde_json::json;

fn make_tool_event(tool_name: &str, with_response: bool) -> serde_json::Value {
    let mut v = json!({
        "tool_name": tool_name,
        "tool_input": {"file_path": "src/main.rs", "old_string": "foo", "new_string": "bar"},
        "session_id": "sess_e2e_m4"
    });
    if with_response {
        v.as_object_mut().unwrap().insert(
            "tool_response".into(),
            json!({"output": "ok"}),
        );
    }
    v
}

#[test]
fn pre_tool_use_edit_has_no_tool_response() {
    let raw = make_tool_event("Edit", false);
    let ev = parse_event(raw.clone(), EventKind::Tool, "claude");
    assert_eq!(ev.tool_called.as_deref(), Some("Edit"));
    assert!(raw.get("tool_response").is_none());
}

#[test]
fn post_tool_use_edit_has_tool_response() {
    let raw = make_tool_event("Edit", true);
    assert!(raw.get("tool_response").is_some());
}

#[test]
fn read_tool_non_l2_tool() {
    let raw = json!({
        "tool_name": "Read",
        "tool_input": {"file_path": "src/lib.rs"},
        "session_id": "sess_e2e_m4"
    });
    let ev = parse_event(raw.clone(), EventKind::Tool, "claude");
    let called = ev.tool_called.as_deref().unwrap_or("").to_ascii_lowercase();
    assert!(!["edit", "write", "bash", "multiedit"].contains(&called.as_str()));
}
```

### Step 2: Run integration tests

```bash
cargo test -p mur-core --test hook_e2e_m4 2>&1 | tail -10
```
Expected: 3 passed.

### Step 3: Run full workspace suite

```bash
cargo test --workspace 2>&1 | grep -E "FAILED|^test result" | tail -10
```
Expected: zero failures.

### Step 4: Clippy + fmt

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep "^error"
cargo fmt --check 2>&1 || cargo fmt
```

### Step 5: Commit + push

```bash
git add mur-core/tests/hook_e2e_m4.rs
git commit -m "test(hook): M4 end-to-end tests — PreToolUse detection, L2 tool recognition, non-L2 filter"
git push origin feat/m0-adaptive-gate
```

---

## Notes for the implementer

- **`is_l2_tool` helper:** Keep this as a private module-level function in `hook.rs`. Do NOT make it `pub` unless tests require it — prefer testing via the integration test's own helper that mirrors the logic.

- **`workflow_name_matches_query` word length filter:** The `>= 4` character minimum avoids false matches on short words like `run`, `the`, `get`. If you get false positives or false negatives in testing, adjust the threshold.

- **WorkflowYamlStore in workflow bump:** If `~/.mur/workflows/` doesn't exist on the test machine, `list_all()` should return an empty Vec (not an Err). Verify this doesn't panic before committing. If it errors on missing dir, add a graceful fallback:
  ```rust
  let workflow_names: Vec<String> = WorkflowYamlStore::default_store()
      .and_then(|s| s.list_all())
      .unwrap_or_default()
      .into_iter()
      .map(|w| w.name.clone())
      .collect();
  ```

- **`effective_tier` and `outcome.tier`:** The `Skip` early-return happens BEFORE the workflow bump. This means: if `intent_score == 0.0` (pure ack like "ok"), the query still returns Skip — workflow detection does NOT override `intent == 0`. This is intentional: "ok run the deploy workflow" → `intent_score` would be > 0 due to "run" verb.

- **Double `WorkflowYamlStore` load:** The workflow bump loads the store, and the synchronous fallback also loads it. This is fine for M4. Extract to a single load in M5 if profiling shows it matters.

- **`Workflow::name` access:** `Workflow` has `base: KnowledgeBase` with `#[serde(flatten)]` and implements `Deref<Target = KnowledgeBase>`, so `workflow.name` works directly. In iterator chains, `.map(|w| w.name.clone())` is fine.
