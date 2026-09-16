# GuardedToolCall extraction Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the guarded tool-call sequence out of `task_runner.rs` into one `GuardedToolCall` unit that owns every obligation, with `TaskRunner` delegating to it and **no new caller** — so the MCP server of `2026-09-16-mur-tool-mcp-server-design.md` has something to call without a second execution path ever existing.

**Architecture:** `GuardedToolCall` holds the nine pieces the guarded sequence reads from `TaskRunner` and gains the two methods that currently live there (`gate_response`, `handle_tool_call`) plus the two helpers they call (`execute_scoped`, `masked`). `TaskRunner` keeps its fields and constructs a `GuardedToolCall` on demand; its two methods become one-line delegations.

**Tech stack:** Rust (edition 2024, `mur-agent-runtime`).

### Global Constraints

- **Pure code movement first; behavior changes in a separate PR.** (CLAUDE.md rule)
- **No second execution path.** After this plan, `ToolExecutor::execute` has exactly one call site in the crate.
- Single source file ≤ 800 lines. (CLAUDE.md rule 4 — `task_runner.rs` is 6795 and this removes ~300.)
- No new caller of `GuardedToolCall` in this plan. The MCP server is a later change.

### Why a pure move, and what that forbids

Every obligation this sequence carries fails **silently** when it is dropped —
a missing mask does not error, it leaks; a missing task scope does not error,
it orphans a `bash` job. So the only safe first step is one where "did the
behaviour change?" has a mechanical answer: the moved bodies are byte-identical
and the tests that covered them still pass.

That forbids, in this plan: renaming anything moved, changing a signature
beyond the receiver, "while we're here" cleanups, and touching the moved code's
comments. Those are all fine — in the next PR.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-agent-runtime/src/tools/guarded.rs` | created | `GuardedToolCall`: the nine fields, the two moved methods, the two moved helpers. The only place `ToolExecutor::execute` is called. |
| `mur-agent-runtime/src/tools/mod.rs` | modified | One `pub mod guarded;` line. |
| `mur-agent-runtime/src/task_runner.rs` | modified | Loses ~300 lines; gains a constructor for `GuardedToolCall` and two delegating methods. |

## What moves, exactly

Line numbers are against `bc77fc01`; verify by symbol name, not by number.

| moved | from | lines |
|---|---|---|
| `execute_scoped` | `task_runner.rs:707` | 12 |
| `masked` | `task_runner.rs:720` | 6 |
| `gate_response` | `task_runner.rs:1802` | 64 |
| `handle_tool_call` | `task_runner.rs:1866` | 267 |

The nine pieces those four read from `TaskRunner`, with their declared types:

```rust
tools: Vec<Arc<dyn crate::tools::ToolExecutor>>
tools_policy: Vec<mur_common::agent::ToolRule>
secrets: Option<Arc<crate::secrets::SecretVault>>
notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>
client_notifiers: Arc<tokio::sync::Mutex<HashMap<String, ApprovalSink>>>
agent_name: String
decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>
hitl_timeout_secs: u32
pending_approvals: Option<HitlApprovals>
```

`tools_for_loop()` is `&self.tools` and does not move; inside `GuardedToolCall`
the body reads `&self.tools` directly.

---

## Task 1 — The struct and its construction, with nothing moved yet

Land the container first, wired and unused, so Task 2 is only movement.

### Interfaces

**Consumes:** nothing.

**Produces:**
```rust
pub struct GuardedToolCall { /* the nine fields above, all pub(crate) */ }
impl TaskRunner { fn guarded(&self) -> GuardedToolCall }
```

### Steps

- [ ] Create `mur-agent-runtime/src/tools/guarded.rs` with the module doc and
      the struct. Copy each field's type verbatim from the table above:

```rust
//! The guarded tool-call sequence: the one place a MUR tool runs.
//!
//! Every obligation here fails silently when it is skipped — a missing mask
//! leaks instead of erroring, a missing task scope orphans a `bash` job
//! instead of erroring. `task_runner.rs` carried two comments warning that a
//! third execution site would break exactly those two things. This type is
//! the answer: one owner, so "is the rule kept?" is a question about one
//! place instead of a question about vigilance.
//!
//! It exists to have a second caller — the MCP server that serves MUR's
//! tools to a spawned CLI — without that caller becoming a second path.
//! See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::hitl::HitlApprovals;

/// Everything the guarded sequence reads. Cloned from `TaskRunner` rather
/// than borrowed: the MCP server needs one that outlives any single turn's
/// borrow, and cloning `Arc`s and a small `Vec` per call is not a cost worth
/// a lifetime parameter here.
pub struct GuardedToolCall {
    pub(crate) tools: Vec<Arc<dyn crate::tools::ToolExecutor>>,
    pub(crate) tools_policy: Vec<mur_common::agent::ToolRule>,
    pub(crate) secrets: Option<Arc<crate::secrets::SecretVault>>,
    pub(crate) notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    pub(crate) client_notifiers:
        Arc<tokio::sync::Mutex<HashMap<String, crate::task_runner::ApprovalSink>>>,
    pub(crate) agent_name: String,
    pub(crate) decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    pub(crate) hitl_timeout_secs: u32,
    pub(crate) pending_approvals: Option<HitlApprovals>,
}
```

- [ ] Add to `mur-agent-runtime/src/tools/mod.rs`, with the other `pub mod`
      lines, in alphabetical position:

```rust
pub mod guarded;
```

- [ ] Add the constructor to `task_runner.rs`, directly above `gate_response`:

```rust
    /// A `GuardedToolCall` over this runner's current configuration.
    ///
    /// Built per call rather than held as a field: `tools` and `tools_policy`
    /// are replaced by the `with_*` builders after construction, so a cached
    /// copy would serve a stale policy — the one kind of staleness that
    /// silently widens what a tool may do.
    fn guarded(&self) -> crate::tools::guarded::GuardedToolCall {
        crate::tools::guarded::GuardedToolCall {
            tools: self.tools.clone(),
            tools_policy: self.tools_policy.clone(),
            secrets: self.secrets.clone(),
            notifier: self.notifier.clone(),
            client_notifiers: self.client_notifiers.clone(),
            agent_name: self.agent_name.clone(),
            decision_store: self.decision_store.clone(),
            hitl_timeout_secs: self.hitl_timeout_secs,
            pending_approvals: self.pending_approvals.clone(),
        }
    }
```

- [ ] Whatever the compiler says is missing, fix by making it available, not
      by changing its type. If `ApprovalSink` or `HitlApprovals` is private,
      widen it to `pub(crate)`; if a field type differs from the table, the
      table is stale — take the type from the code and note the difference in
      the PR.

- [ ] Build and watch it compile with the struct unused:

```bash
cargo build -p mur-agent-runtime
```

Expected: compiles; a `dead_code` warning naming `guarded` is correct at this
point and disappears in Task 2.

- [ ] Commit: `git add mur-agent-runtime/src/tools/guarded.rs mur-agent-runtime/src/tools/mod.rs mur-agent-runtime/src/task_runner.rs && git commit -m "refactor(runtime): GuardedToolCall container, nothing moved yet"`

---

## Task 2 — Move the four bodies, unchanged

### Interfaces

**Consumes:** `GuardedToolCall` (Task 1).

**Produces:**
```rust
impl GuardedToolCall {
    pub(crate) async fn execute_scoped(
        tool: &dyn crate::tools::ToolExecutor,
        task_id: &str,
        input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError>;
    pub(crate) fn masked(&self, output: String) -> String;
    pub(crate) async fn gate_response(
        &self,
        task_id: &str,
        calls: &[crate::llm::ToolCallResult],
    ) -> (HashMap<String, crate::hitl::HitlDecision>, HashMap<String, String>);
    pub(crate) async fn handle_tool_call(
        &self,
        task_id: &str,
        call: &crate::llm::ToolCallResult,
        decision: Option<crate::hitl::HitlDecision>,
        step_id: Option<String>,
    ) -> Result<crate::llm::ToolResultEntry, crate::task_runner::TaskError>;
}
```

Signatures are the current ones verbatim. The receiver is the only thing that
changes, and only because the type did.

### Steps

- [ ] Cut `execute_scoped`, `masked`, `gate_response` and `handle_tool_call`
      from `task_runner.rs` and paste them into an `impl GuardedToolCall`
      block in `guarded.rs`. **Do not retype them.** Do not reformat, rename,
      reorder or reword a comment.

- [ ] Fix only what the move itself breaks:
  - `self.tools_for_loop()` → `&self.tools` (the helper stays on `TaskRunner`).
  - `Self::execute_scoped` still resolves — `Self` is now `GuardedToolCall`.
  - Paths that were crate-relative from `task_runner` may need `crate::`
    prefixes. Adding a path is not a behaviour change; changing a call is.

- [ ] Replace the two methods in `task_runner.rs` with delegations:

```rust
    async fn gate_response(
        &self,
        task_id: &str,
        calls: &[crate::llm::ToolCallResult],
    ) -> (
        HashMap<String, crate::hitl::HitlDecision>,
        HashMap<String, String>,
    ) {
        self.guarded().gate_response(task_id, calls).await
    }

    async fn handle_tool_call(
        &self,
        task_id: &str,
        call: &crate::llm::ToolCallResult,
        decision: Option<crate::hitl::HitlDecision>,
        step_id: Option<String>,
    ) -> Result<crate::llm::ToolResultEntry, TaskError> {
        self.guarded()
            .handle_tool_call(task_id, call, decision, step_id)
            .await
    }
```

- [ ] Run the runtime's whole suite and watch it pass unchanged:

```bash
cargo test -p mur-agent-runtime
```

Expected: the same pass count as on `bc77fc01`. Record both numbers in the PR.
A pure move that changes a count changed behaviour — find out why before
continuing.

- [ ] Lint and format:

```bash
cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt --check
```

Expected: no output, exit 0.

- [ ] Commit: `git add -A && git commit -m "refactor(runtime): move the guarded tool-call sequence into GuardedToolCall"`

---

## Task 3 — Make the constraint mechanical

The design's hard constraint is "no second execution path". Until a test
enforces it, it is a sentence — and `task_runner.rs` already demonstrates what
sentences achieve: two comments begging a future reader not to add a third
site, which is precisely the thing a test can check and prose cannot.

### Interfaces

**Consumes:** `GuardedToolCall` with the bodies moved (Task 2).

**Produces:** a test that fails if `ToolExecutor::execute` is called anywhere
but `guarded.rs`.

### Steps

- [ ] Append to `mur-agent-runtime/src/tools/guarded.rs`:

```rust
#[cfg(test)]
mod tests {
    /// `ToolExecutor::execute` may be called from exactly one file.
    ///
    /// Not style. Each call site has to carry the policy gate, the HITL
    /// decision, the task scope and the secret mask, and every one of those
    /// fails silently when it is missed — the output is simply unmasked, the
    /// job simply has no owner. This is the check that was missing when
    /// `task_runner.rs` resorted to asking in comments.
    ///
    /// If this fails, the fix is to route the new caller through
    /// `GuardedToolCall`, not to widen the test.
    #[test]
    fn execute_is_called_from_guarded_only() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut stack = vec![src.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read_dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // The owner, and the trait's own definition and impls.
                if path.ends_with("tools/guarded.rs") || path.starts_with(src.join("tools")) {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read");
                for (i, line) in text.lines().enumerate() {
                    if line.contains(".execute(") {
                        offenders.push(format!("{}:{}", path.display(), i + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "ToolExecutor::execute called outside tools/guarded.rs — route it \
             through GuardedToolCall instead:\n{}",
            offenders.join("\n")
        );
    }
}
```

- [ ] Run it and watch it pass:

```bash
cargo test -p mur-agent-runtime execute_is_called_from_guarded_only
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] Prove the test can fail — a guard that cannot fail is not a guard. The
      scan reads text, so a comment is enough and it compiles. Add this line
      temporarily at the top of `mur-agent-runtime/src/supervisor.rs`:

```rust
// tripwire check: .execute(
```

- [ ] Run the test again and watch it FAIL, naming that file and line:

```bash
cargo test -p mur-agent-runtime execute_is_called_from_guarded_only
```

Expected: a failure listing `supervisor.rs:1`.

That a *comment* trips it is the test's real nature, not a defect to file
down: it is a textual scan, so it over-reports rather than under-reports. For
a guard whose whole job is to refuse a silent new execution path, a false
alarm someone must justify beats a miss nobody sees.

- [ ] Remove the temporary line, re-run, watch it pass again.

- [ ] Commit: `git add mur-agent-runtime/src/tools/guarded.rs && git commit -m "test(runtime): execute() may be called from one file only"`

## Done when

- [ ] `cargo test -p mur-agent-runtime` — same pass count as `bc77fc01`.
- [ ] `cargo test -p mur-agent-runtime execute_is_called_from_guarded_only` — 1 passed.
- [ ] `cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt --check` — clean.
- [ ] `task_runner.rs` is ~300 lines shorter and still over 800 — this pays down
      part of rule 4, not all of it; say so rather than implying otherwise.
- [ ] `GuardedToolCall` has exactly one caller. `grep -rn "\.guarded()" src/`
      returns only `task_runner.rs`.

## Not in this plan

- **The MCP server.** It is the reason this extraction exists, and it is a
  separate change with its own design.
- **Re-entrancy.** `gate_response` takes a slice and is already driven with a
  one-element slice by existing tests, so the shape fits a single `tools/call`.
  What is unverified is *concurrent* invocation, and nothing here introduces
  any — the sole caller is still a sequential turn loop. Open question 1 of
  the MCP design stays open, and this plan deliberately does not pretend to
  close it.
- **Splitting `task_runner.rs` to 800 lines.** Out of scope; this removes ~300
  of 6795.
