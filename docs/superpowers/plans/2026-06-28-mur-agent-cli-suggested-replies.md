# Agent-suggested replies (`suggest_replies` tool) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an agent offer the user 1–5 Tab-to-fill quick replies via a no-op `suggest_replies` tool, rendered in the `mur agent cli` TUI as ghost text (one) or the Type-1 completion overlay (many).

**Architecture:** A built-in no-op `suggest_replies` tool in `mur-agent-runtime` (auto-approved, offered to the model only on streaming/interactive turns). Its call streams to the TUI as the existing `StreamMsg::StepStarted`; the TUI intercepts it by name, stashes the replies, and reveals them after the turn ends when the composer is empty — a single reply becomes the input placeholder (ghost), multiple reuse the Type-1 `CompletionState` overlay. All decision logic lives in pure functions; runtime↔TUI plumbing is thin glue.

**Tech Stack:** Rust 2024. `mur-agent-runtime` (tool + runtime gate) and `mur-core` (TUI). ratatui / `tui_textarea` for the input. No A2A envelope changes.

## Global Constraints

- Tool name is exactly `suggest_replies`; input schema is `{replies: string[], minItems 1, maxItems 5}`.
- The tool is **no-op** (no side effects) and **auto-approved** (never triggers HITL).
- It is offered to the model **only on streaming (interactive) turns** — non-streaming turns (`mur agent send`, fleet) must not see it.
- Suggestions are revealed **only after the turn finishes and only when the input is empty**.
- No new dependencies. Single shared name constant `SUGGEST_REPLIES`.
- **Env for builds/tests** (external drive + rust-embed):
  - `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`
  - `export ORT_STRATEGY=download`
  - `export MUR_WEB_DIST="$HOME/Projects/mur-web/dist"` (required for `mur-core` to link; harmless for `mur-agent-runtime`).
  - Use plain `cargo test -p <crate> <filter>` (NOT nextest, NOT `--workspace`). Builds are slow; prefer `cargo check`/scoped tests.
- Spec: `docs/superpowers/specs/2026-06-28-mur-agent-cli-suggested-replies-design.md`.

---

## File Structure

- **Create** `mur-agent-runtime/src/tools/suggest.rs` — `SUGGEST_REPLIES` const, `SuggestRepliesTool` (no-op `ToolExecutor`), `offer_for_streaming()` gate predicate, `suggest_replies_allowed()` policy helper. Unit-tested.
- **Modify** `mur-agent-runtime/src/tools/mod.rs` — declare `mod suggest;`.
- **Modify** `mur-agent-runtime/src/tools/registry.rs` — register the tool in `build_tools`.
- **Modify** `mur-agent-runtime/src/task_runner.rs` — policy exemption (1088 region) + per-turn streaming gate (1317).
- **Create** `mur-core/src/cmd/agent/cli/suggest.rs` — `parse_suggestions()`, `Reveal` enum, `plan_reveal()`. Pure, unit-tested.
- **Modify** `mur-core/src/cmd/agent/cli/mod.rs` — declare `mod suggest;`; intercept `StepStarted`; reveal on `Done`; ghost-fill key handling.
- **Modify** `mur-core/src/cmd/agent/cli/app.rs` — two fields + `reveal_suggestions()` / `clear_suggestion_ghost()`.

---

## Task 1: Runtime `suggest_replies` tool module

**Files:**
- Create: `mur-agent-runtime/src/tools/suggest.rs`
- Modify: `mur-agent-runtime/src/tools/mod.rs` (add `mod suggest;`)

**Interfaces:**
- Produces:
  - `pub const SUGGEST_REPLIES: &str = "suggest_replies";`
  - `pub struct SuggestRepliesTool;` implementing `crate::tools::ToolExecutor`
  - `pub fn offer_for_streaming(name: &str, streaming: bool) -> bool`
  - `pub fn suggest_replies_allowed(name: &str) -> bool` (true iff this is the suggest tool — caller maps it to `ToolPolicy::Allow`)

- [ ] **Step 1: Declare the module**

In `mur-agent-runtime/src/tools/mod.rs`, add alongside the other `mod` lines (e.g. after `mod bash;`):

```rust
mod suggest;
```

- [ ] **Step 2: Write the failing test + skeleton**

Create `mur-agent-runtime/src/tools/suggest.rs`:

```rust
//! `suggest_replies` — a no-op tool the agent calls to offer the user 1–5
//! Tab-to-fill quick replies. The user-facing effect is carried entirely by the
//! streamed tool-call args (`StepStarted`); the executor itself does nothing and
//! returns a bare acknowledgement so the model can finish its turn.

use super::{ToolError, ToolExecutor};
use crate::llm::ToolDef;

/// Canonical tool name. Shared by the runtime gate and the TUI interceptor.
pub const SUGGEST_REPLIES: &str = "suggest_replies";

pub struct SuggestRepliesTool;

#[async_trait::async_trait]
impl ToolExecutor for SuggestRepliesTool {
    fn name(&self) -> &str {
        SUGGEST_REPLIES
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: SUGGEST_REPLIES.into(),
            description: "Offer the user 1-5 short quick-reply options when they \
                would likely pick from a small set — e.g. after you ask a question \
                or propose a choice. Each option is a complete message the user \
                could send verbatim. The options are shown as Tab-to-fill \
                suggestions in the user's input; calling this does NOT end your \
                turn. Do not call it for open-ended turns with no natural shortlist."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "replies": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 5,
                        "description": "1-5 short complete messages the user could send."
                    }
                },
                "required": ["replies"]
            }),
        }
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ToolError> {
        // No side effects: the replies reach the user via the streamed args.
        Ok("ok".to_string())
    }
}

/// Whether `name` should be offered to the model this turn. Everything is
/// offered normally; `suggest_replies` is offered only on streaming
/// (interactive) turns so non-interactive callers never see it.
pub fn offer_for_streaming(name: &str, streaming: bool) -> bool {
    streaming || name != SUGGEST_REPLIES
}

/// `suggest_replies` is a no-side-effect built-in and is always auto-approved,
/// regardless of the agent's default tool policy.
pub fn suggest_replies_allowed(name: &str) -> bool {
    name == SUGGEST_REPLIES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execute_is_noop_ok() {
        let out = SuggestRepliesTool
            .execute(serde_json::json!({ "replies": ["yes", "no"] }))
            .await;
        assert!(out.is_ok());
    }

    #[test]
    fn def_has_canonical_name_and_schema() {
        let d = SuggestRepliesTool.def();
        assert_eq!(d.name, "suggest_replies");
        assert_eq!(d.input_schema["properties"]["replies"]["type"], "array");
    }

    #[test]
    fn streaming_gate() {
        assert!(offer_for_streaming("suggest_replies", true));
        assert!(!offer_for_streaming("suggest_replies", false));
        // Other tools are always offered.
        assert!(offer_for_streaming("bash", false));
        assert!(offer_for_streaming("bash", true));
    }

    #[test]
    fn policy_exemption_only_for_suggest() {
        assert!(suggest_replies_allowed("suggest_replies"));
        assert!(!suggest_replies_allowed("bash"));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download cargo test -p mur-agent-runtime suggest`
Expected: PASS (4 tests). (`async_trait`, `serde_json`, `tokio` are already deps of the crate.)

- [ ] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/tools/suggest.rs mur-agent-runtime/src/tools/mod.rs
git commit -m "feat(runtime): suggest_replies no-op tool + gate/policy helpers"
```

---

## Task 2: Register the tool + wire the gate & policy

**Files:**
- Modify: `mur-agent-runtime/src/tools/registry.rs` (`build_tools`, the built-in block ~lines 33–38)
- Modify: `mur-agent-runtime/src/task_runner.rs` (policy gate ~line 1088; per-turn `tool_defs` ~line 1317)

**Interfaces:**
- Consumes: `crate::tools::suggest::{SUGGEST_REPLIES, SuggestRepliesTool, offer_for_streaming, suggest_replies_allowed}` from Task 1.

- [ ] **Step 1: Register the tool in `build_tools`**

In `mur-agent-runtime/src/tools/registry.rs`, immediately after the `if let Some((def, exec)) = bash … { … }` built-in block (around line 38, before the `let discovery_futs` line), insert:

```rust
    // Built-in no-op suggest_replies. Always in the executor map (so a call can
    // execute); per-turn streaming gating happens in task_runner. Respect an
    // explicit Deny rule.
    if resolve_tool_policy(rules, crate::tools::suggest::SUGGEST_REPLIES) != ToolPolicy::Deny {
        let exec: std::sync::Arc<dyn ToolExecutor> =
            std::sync::Arc::new(crate::tools::suggest::SuggestRepliesTool);
        defs.push(exec.def());
        map.insert(crate::tools::suggest::SUGGEST_REPLIES.to_string(), exec);
    }
```

(`resolve_tool_policy`, `ToolPolicy`, `ToolExecutor`, `Arc` are already in scope in this file — the `bash` block above uses them. If `Arc`/`ToolExecutor` are imported unqualified there, drop the `std::sync::`/path prefixes to match.)

- [ ] **Step 2: Verify it compiles + the existing registry test sees the tool**

Add to the `#[cfg(test)] mod tests` in `registry.rs` (mirror the existing `build_tools(None, &[], &[], pool)` test — reuse its pool setup):

```rust
    #[tokio::test]
    async fn suggest_replies_is_registered() {
        let pool = std::sync::Arc::new(crate::tools::mcp::McpPool::default());
        let (defs, map) = build_tools(None, &[], &[], pool).await;
        assert!(map.contains_key("suggest_replies"));
        assert!(defs.iter().any(|d| d.name == "suggest_replies"));
    }
```

If `McpPool::default()` is not the constructor used by the neighbouring test, copy that test's exact pool construction instead.

Run: `PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download cargo test -p mur-agent-runtime registry`
Expected: PASS, including `suggest_replies_is_registered`.

- [ ] **Step 3: Auto-approve the tool (policy exemption)**

In `mur-agent-runtime/src/task_runner.rs`, in `handle_tool_call`, replace the policy resolution at the `match resolve_tool_policy(&self.tools_policy, &call.tool_name)` site (~line 1088). Change:

```rust
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};
        match resolve_tool_policy(&self.tools_policy, &call.tool_name) {
```

to:

```rust
        use mur_common::agent::{ToolPolicy, resolve_tool_policy};
        let policy = if crate::tools::suggest::suggest_replies_allowed(&call.tool_name) {
            ToolPolicy::Allow
        } else {
            resolve_tool_policy(&self.tools_policy, &call.tool_name)
        };
        match policy {
```

(Everything inside the `match` arms is unchanged.)

- [ ] **Step 4: Gate the tool to streaming turns**

In `mur-agent-runtime/src/task_runner.rs`, `run_agentic_loop`, change the `tool_defs` construction at line 1317:

```rust
        let tool_defs: Vec<_> = self.tools_for_loop().iter().map(|t| t.def()).collect();
```

to:

```rust
        // `suggest_replies` is offered to the model only on streaming
        // (interactive) turns — non-interactive callers never see it.
        let streaming = sink.is_some();
        let tool_defs: Vec<_> = self
            .tools_for_loop()
            .iter()
            .map(|t| t.def())
            .filter(|d| crate::tools::suggest::offer_for_streaming(&d.name, streaming))
            .collect();
```

- [ ] **Step 5: Verify the crate compiles & lints**

Run: `PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download cargo clippy -p mur-agent-runtime -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/tools/registry.rs mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): register suggest_replies, auto-approve it, gate to streaming turns"
```

---

## Task 3: TUI pure suggestion logic (`cli/suggest.rs`)

**Files:**
- Create: `mur-core/src/cmd/agent/cli/suggest.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (add `mod suggest;`)

**Interfaces:**
- Produces (used by Tasks 4–5):
  - `pub fn parse_suggestions(args: &serde_json::Value) -> Vec<String>`
  - `pub enum Reveal { None, Ghost(String), Chooser(Vec<String>) }`
  - `pub fn plan_reveal(pending: Vec<String>, input_empty: bool) -> Reveal`
  - `pub const MAX_SUGGESTIONS: usize = 5;`

- [ ] **Step 1: Declare the module**

In `mur-core/src/cmd/agent/cli/mod.rs`, add alongside the other `mod` lines (e.g. after `mod step;`):

```rust
mod suggest;
```

- [ ] **Step 2: Write the failing tests + implementation**

Create `mur-core/src/cmd/agent/cli/suggest.rs`:

```rust
//! Pure logic for agent-suggested replies: extract the `replies` array from a
//! `suggest_replies` tool call, and decide how to reveal them. No TUI, no I/O.

/// Hard cap on suggestions shown (matches the tool schema's maxItems).
pub const MAX_SUGGESTIONS: usize = 5;

/// Extract non-empty reply strings from the tool-call args, capped. Fail-soft:
/// any malformed shape yields an empty vec.
pub fn parse_suggestions(args: &serde_json::Value) -> Vec<String> {
    let Some(arr) = args.get("replies").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .take(MAX_SUGGESTIONS)
        .map(str::to_string)
        .collect()
}

/// How to surface a set of pending suggestions.
#[derive(Debug, Clone, PartialEq)]
pub enum Reveal {
    /// Nothing to show (empty, or the composer already has text).
    None,
    /// A single suggestion → ghost placeholder text.
    Ghost(String),
    /// Two or more → a chooser overlay.
    Chooser(Vec<String>),
}

/// Decide how to reveal `pending` given whether the composer is empty.
pub fn plan_reveal(pending: Vec<String>, input_empty: bool) -> Reveal {
    if pending.is_empty() || !input_empty {
        return Reveal::None;
    }
    if pending.len() == 1 {
        Reveal::Ghost(pending.into_iter().next().unwrap())
    } else {
        Reveal::Chooser(pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_extracts_trims_and_drops_empties() {
        let v = parse_suggestions(&json!({ "replies": ["  open PR  ", "", "push"] }));
        assert_eq!(v, vec!["open PR".to_string(), "push".to_string()]);
    }

    #[test]
    fn parse_caps_at_five() {
        let v = parse_suggestions(&json!({ "replies": ["1","2","3","4","5","6","7"] }));
        assert_eq!(v.len(), 5);
    }

    #[test]
    fn parse_malformed_is_empty() {
        assert!(parse_suggestions(&json!({})).is_empty());
        assert!(parse_suggestions(&json!({ "replies": "nope" })).is_empty());
        assert!(parse_suggestions(&json!({ "replies": [1, 2] })).is_empty());
    }

    #[test]
    fn reveal_single_is_ghost() {
        assert_eq!(
            plan_reveal(vec!["only".into()], true),
            Reveal::Ghost("only".into())
        );
    }

    #[test]
    fn reveal_many_is_chooser() {
        assert_eq!(
            plan_reveal(vec!["a".into(), "b".into()], true),
            Reveal::Chooser(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn reveal_skips_when_input_not_empty_or_pending_empty() {
        assert_eq!(plan_reveal(vec!["a".into()], false), Reveal::None);
        assert_eq!(plan_reveal(Vec::new(), true), Reveal::None);
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" cargo test -p mur-core suggest`
Expected: PASS (6 tests). (Builds may be slow; if the test binary compile exceeds the shell timeout, run with `--lib`: `... cargo test -p mur-core --lib suggest`.)

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/cli/suggest.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): pure logic for agent-suggested replies (parse + reveal plan)"
```

---

## Task 4: TUI — App state, reveal, and stream interception

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (struct fields tail; `App::new` tail; new methods)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`handle_stream`: `StepStarted` + `Done`)

**Interfaces:**
- Consumes: `super::suggest::{Reveal, parse_suggestions, plan_reveal}`; `super::complete::{Candidate, CompletionState}`.
- Produces: `App.pending_suggestions: Vec<String>`, `App.suggestion_ghost: Option<String>`, `App::reveal_suggestions()`, `App::clear_suggestion_ghost()`.

- [ ] **Step 1: Add the two fields**

In `app.rs`, in the `App` struct, immediately after `pub skills: Vec<Candidate>,` (the last field):

```rust
    /// Replies captured from a `suggest_replies` tool call this turn, revealed
    /// after the turn finishes (see `reveal_suggestions`).
    pub pending_suggestions: Vec<String>,
    /// The single suggestion currently shown as ghost placeholder text, if any.
    pub suggestion_ghost: Option<String>,
```

In `App::new`, immediately after `skills: Vec::new(),`:

```rust
            pending_suggestions: Vec::new(),
            suggestion_ghost: None,
```

- [ ] **Step 2: Add the reveal + clear methods**

In `app.rs`, add to `impl App` (near `set_input`/`clear_input`):

```rust
    /// Reveal suggestions captured this turn: one → ghost placeholder, many →
    /// the completion overlay. No-op unless the composer is empty. Clears
    /// `pending_suggestions` either way.
    pub fn reveal_suggestions(&mut self) {
        let pending = std::mem::take(&mut self.pending_suggestions);
        let input_empty = self.input_text().is_empty();
        self.clear_suggestion_ghost();
        match super::suggest::plan_reveal(pending, input_empty) {
            super::suggest::Reveal::None => {}
            super::suggest::Reveal::Ghost(text) => {
                self.input.set_placeholder_text(&text);
                self.suggestion_ghost = Some(text);
            }
            super::suggest::Reveal::Chooser(items) => {
                self.completion = Some(super::complete::CompletionState {
                    items: items
                        .into_iter()
                        .map(|s| super::complete::Candidate {
                            display: s.clone(),
                            insert: s,
                            desc: String::new(),
                            has_children: false,
                        })
                        .collect(),
                    selected: 0,
                });
            }
        }
    }

    /// Drop any ghost suggestion and restore the default placeholder.
    pub fn clear_suggestion_ghost(&mut self) {
        if self.suggestion_ghost.take().is_some() {
            self.input.set_placeholder_text("Type a message…");
        }
    }
```

Note: `set_placeholder_text` accepts `impl Into<String>`; `&text` / `"Type a message…"` both work (matches `new_input`).

- [ ] **Step 3: Intercept the tool call + reveal on Done (mod.rs)**

In `handle_stream`, replace the `StreamMsg::StepStarted` arm (currently lines ~1036–1044):

```rust
        StreamMsg::StepStarted {
            step_id,
            name,
            args,
            ..
        } => {
            app.saw_step_this_turn = true;
            app.push_step_started(step_id, name, args);
        }
```

with:

```rust
        StreamMsg::StepStarted {
            step_id,
            name,
            args,
            ..
        } => {
            if name == suggest::SUGGEST_REPLIES_NAME {
                // No step card: stash the replies for reveal at turn end.
                app.pending_suggestions = suggest::parse_suggestions(&args);
            } else {
                app.saw_step_this_turn = true;
                app.push_step_started(step_id, name, args);
            }
        }
```

This references a TUI-side name constant. Add it to `cli/suggest.rs` (Task 3's module) so the TUI doesn't depend on the runtime crate — add this line near `MAX_SUGGESTIONS`:

```rust
/// The runtime tool name the TUI intercepts (mirrors mur-agent-runtime's
/// `tools::suggest::SUGGEST_REPLIES`). Kept in sync by the spec; both are the
/// literal string "suggest_replies".
pub const SUGGEST_REPLIES_NAME: &str = "suggest_replies";
```

Then, at the **end** of the `StreamMsg::Done` arm (after the `match stream::task_outcome(&task) { … }` block, still inside the arm), add:

```rust
            app.reveal_suggestions();
```

- [ ] **Step 4: Verify compile + existing tests still pass**

Run: `PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" cargo check -p mur-core`
Expected: PASS. Then the suggest tests still pass:
`... cargo test -p mur-core --lib suggest` → PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/suggest.rs
git commit -m "feat(cli): intercept suggest_replies, reveal ghost/chooser after turn"
```

---

## Task 5: TUI — ghost-fill key handling + clearing

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (key handler; `submit`)

**Interfaces:**
- Consumes: `App.suggestion_ghost`, `App::clear_suggestion_ghost`, `App::set_input`, `App::input_text`.

The multi-suggestion chooser needs **no new key code** — it lives in `app.completion`, handled by the existing Type-1 `if app.completion.is_some() { … }` interceptor (↑↓/Ctrl+P/N move, Tab/Enter accept, Esc close). This task only adds the single-suggestion ghost `Tab`-fill and the clearing.

- [ ] **Step 1: Add the ghost-fill interceptor**

In `mod.rs` `handle_event`, immediately AFTER the `if app.completion.is_some() { … }` block (so the chooser keeps priority) and BEFORE the main `match key.code {`:

```rust
            // Agent ghost suggestion: Tab fills it when the composer is empty.
            if app.suggestion_ghost.is_some()
                && key.code == KeyCode::Tab
                && app.input_text().is_empty()
            {
                if let Some(s) = app.suggestion_ghost.take() {
                    app.set_input(&s);
                    app.input.set_placeholder_text("Type a message…");
                }
                return;
            }
```

(When the composer is non-empty the guard is skipped, so `Tab` falls through to the existing `refresh_completion` / slash menu. `tui_textarea` only paints the placeholder while empty, so a ghost is never visible once the user types.)

- [ ] **Step 2: Clear the ghost on submit**

In `mod.rs`, at the very start of the `async fn submit(app: &mut App, …)` function (the one the `KeyCode::Enter => submit(app, tx).await` arm calls), add:

```rust
    app.clear_suggestion_ghost();
```

(So sending a message — whether the ghost text or something else — drops the stale ghost; the next turn's `reveal_suggestions` sets a fresh one.)

- [ ] **Step 3: Verify compile + full gate**

Run:
```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo fmt -p mur-core && cargo fmt -p mur-core -- --check \
  && cargo clippy -p mur-core -- -D warnings \
  && cargo test -p mur-core --lib suggest
```
Expected: fmt clean, clippy clean, suggest tests PASS. Also run runtime fmt/clippy:
`... cargo fmt -p mur-agent-runtime -- --check && cargo clippy -p mur-agent-runtime -- -D warnings` → clean.

- [ ] **Step 4: Manual smoke test (TUI can't be unit-tested)**

Build and drive a real interactive agent in tmux (headless can't drive a TUI):
```bash
PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo build -p mur-core --bin mur
```
Then run `./target/debug/mur agent cli <agent>` and prompt the agent in a way that elicits a choice (e.g. "give me two options for X and offer them as quick replies"). Confirm: a single suggestion shows as greyed placeholder and `Tab` fills it; multiple show the overlay and `↑↓`/`Tab` pick; typing dismisses; `Enter` sends. (Operator-verified, as with Type 1.)

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): Tab-fill agent ghost suggestion; clear on submit"
```

---

## Self-Review notes (reconciled)

- **Spec coverage:** no-op `suggest_replies` tool with the exact schema (T1) · auto-approved / no HITL (T2 policy exemption) · streaming-only offering (T2 gate) · `StepStarted` interception, no step card (T4) · reveal-on-`Done`-when-empty (T4) · ghost for one / Type-1 overlay for many (T4 `reveal_suggestions` + T5 keys) · typing/submit dismiss (T5 + empty-guard) · fail-soft parse (`parse_suggestions`) · old-TUI/old-runtime compatibility is inherent (tool absent → nothing; unknown step card → `update_step_completed` no-ops on a missing id).
- **Type consistency:** `SUGGEST_REPLIES` (runtime) and `SUGGEST_REPLIES_NAME` (TUI) are both the literal `"suggest_replies"`; `Candidate`/`CompletionState` fields (`display`/`insert`/`desc`/`has_children`/`items`/`selected`) match `complete.rs`; `Reveal`/`plan_reveal`/`parse_suggestions` defined in T3 and used by their exact names in T4.
- **Placeholder scan:** none — every step shows full code.
- **Known minor (intentional):** the two name constants are kept in sync by hand (the TUI deliberately does not depend on the runtime crate); a deferred follow-up could move the literal to `mur-common`. A ghost re-appears if the user types then deletes back to empty (the empty-guard prevents any wrong fill) — acceptable for v1.
