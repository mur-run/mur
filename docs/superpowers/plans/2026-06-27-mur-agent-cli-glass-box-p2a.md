# mur agent cli — Glass Box P2a (Inline HITL + Edit Diffs) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move tool approval from a centered modal onto the tool-call card in context, and render a real `-`/`+` diff in the body of edit-tool cards.

**Architecture:** The runtime adds the existing `step_id` to its `tool/approval_needed` notification so the cli can correlate an approval with the exact card that ran the tool; the cli marks that `StepCard` "awaiting approval" and renders `[y]/[a]/[n]` inline (the centered modal stays only as the old-runtime fallback). A new `cli/diff.rs` turns an edit tool's `{file_path, old_string, new_string}` args into red/removed, green/added, dim/context lines via the already-present `diff` crate; `render_card` draws that instead of raw JSON for edit-like tools.

**Tech Stack:** Rust (edition 2024), ratatui + crossterm, serde_json, `diff = "0.1"` (already a mur-core dependency, currently unused).

## Global Constraints

- **Builds on P1** — branch from `feat/agent-cli-glass-box` (PR #517, not yet merged). All P1 types exist: `StepCard`, `card_lines`, `StreamMsg::Step*`, the footer, `HitlRequest`.
- **Rust edition 2024**; no hardcoded values (named `const`); brand "MUR" uppercase in user-facing strings.
- **Single source file ≤ 800 lines** — new code in new modules (`cli/diff.rs`), not by growing `mod.rs`/`ui.rs`.
- **Tests:** rustup proxy is often broken in agent sessions — if `cargo` is not on PATH use `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"` and plain `cargo test` (the `cargo-nextest` binary is absent). mur-core needs `ORT_STRATEGY=download`.
- **Lint gate:** `cargo clippy -p mur-core -p mur-agent-runtime -- -D warnings` + `cargo fmt`.
- **mur-core MUST NOT depend on mur-agent-runtime.**
- **Backward-compat:** an old runtime that omits `step_id` in the approval must still get an approval prompt — the centered modal is the fallback when `step_id` is absent. The `y/a/n` keys act on `app.hitl` and are unchanged either way.
- **Post-hoc HITL note (mur's existing model, not changed here):** the runtime *executes* a tool, emits `step/completed` (card shows `✔` + output), *then* requests approval. So the inline prompt appears on an already-completed card ("ran this — approve keeping it? [y/n]"). The diff is built from args regardless of timing.

---

### Task 1: Runtime adds `step_id` to the approval notification

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs:1169-1181` (the `tool/approval_needed` json)

**Interfaces:**
- Produces (wire): `tool/approval_needed` params gain `"step_id"` (string) — the same `step_id` already emitted in `step/started`/`step/completed` for this call.
- Consumes: `step_id` (already in scope, generated at `task_runner.rs:1048`).

- [ ] **Step 1: Add the field**

The notification currently is:
```rust
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool/approval_needed",
            "params": {
                "hitl_id": hitl_id,
                "task_id": task_id,
                "tool_name": call.tool_name,
                "tool_input": call.input,
                "output": output,
                "is_error": is_error,
                "timeout_ms": (self.hitl_timeout_secs as u64) * 1000,
            }
        });
```
Add `"step_id": step_id,` as the first param (it's in scope from line 1048):
```rust
            "params": {
                "step_id": step_id,
                "hitl_id": hitl_id,
                "task_id": task_id,
                "tool_name": call.tool_name,
                "tool_input": call.input,
                "output": output,
                "is_error": is_error,
                "timeout_ms": (self.hitl_timeout_secs as u64) * 1000,
            }
```

- [ ] **Step 2: Verify build + commit**

Run: `cargo check -p mur-agent-runtime && cargo clippy -p mur-agent-runtime -- -D warnings && cargo fmt`
Expected: clean. (This is a 1-field add to an existing notification; it's exercised end-to-end by the cli correlation tests in later tasks and manual run. No unit test for the inline json.)

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(runtime): include step_id in tool/approval_needed so the cli can attach approval to its card"
```

---

### Task 2: `HitlRequest` carries `step_id`

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/stream.rs:140-168` (`HitlRequest` struct + `from_value`)
- Test: `mur-core/src/cmd/agent/cli/stream.rs` (inline `#[cfg(test)] mod hitl_step_tests`)

**Interfaces:**
- Produces: `HitlRequest.step_id: Option<String>`.
- Consumes: Task 1's wire field.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod hitl_step_tests {
    use super::HitlRequest;

    #[test]
    fn parses_step_id_when_present() {
        let v = serde_json::json!({
            "step_id": "s-1", "hitl_id": "h-1", "tool_name": "edit",
            "tool_input": { "file_path": "a.rs" }, "prompt": "Run `edit`?"
        });
        let req = HitlRequest::from_value(v);
        assert_eq!(req.step_id.as_deref(), Some("s-1"));
        assert_eq!(req.hitl_id, "h-1");
    }

    #[test]
    fn step_id_none_on_old_runtime() {
        let v = serde_json::json!({ "hitl_id": "h-1", "tool_name": "bash" });
        let req = HitlRequest::from_value(v);
        assert!(req.step_id.is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::stream::hitl_step_tests`
Expected: FAIL — `no field step_id`.

- [ ] **Step 3: Add the field + parse it**

In the struct, add after `hitl_id`:
```rust
    /// The `step_id` of the card that ran this tool (P2: lets the cli show the
    /// approval inline on that card). `None` on runtimes predating the field.
    pub step_id: Option<String>,
```
In `from_value`, add to the `Self { … }`:
```rust
            step_id: v
                .get("step_id")
                .and_then(Value::as_str)
                .map(str::to_string),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::stream::hitl_step_tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/stream.rs
git commit -m "feat(cli): HitlRequest carries step_id for inline approval correlation"
```

---

### Task 3: `StepCard.awaiting_hitl` + App correlation

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/step.rs:16-71` (add `awaiting_hitl` field + init)
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (add `mark_card_awaiting` / `clear_card_awaiting`)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs:791-800` (Hitl arm sets it) and `mod.rs:528-552` (`decide_hitl_with_note` clears it)
- Test: `mur-core/src/cmd/agent/cli/app.rs` (inline `#[cfg(test)] mod awaiting_tests`)

**Interfaces:**
- Produces: `StepCard.awaiting_hitl: bool`; `App::mark_card_awaiting(&mut self, step_id: &str)`; `App::clear_card_awaiting(&mut self, step_id: &str)`.
- Consumes: `HitlRequest.step_id` (Task 2), `StepCard.id` (existing).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod awaiting_tests {
    use super::*;

    #[test]
    fn mark_and_clear_awaiting_by_step_id() {
        let mut a = App::test_fixture();
        a.begin_user_turn("edit it");
        a.push_step_started("s1".into(), "edit".into(), serde_json::json!({"file_path":"a.rs"}));
        a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5);
        a.mark_card_awaiting("s1");
        let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
        assert!(card.awaiting_hitl);
        a.clear_card_awaiting("s1");
        let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
        assert!(!card.awaiting_hitl);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::app::awaiting_tests`
Expected: FAIL — `no field awaiting_hitl` / `no method mark_card_awaiting`.

- [ ] **Step 3: Add the field to `StepCard`**

In the struct (after `duration_ms`):
```rust
    /// True while this card's tool call is waiting on a HITL decision (P2 inline
    /// approval). Set when the matching `tool/approval_needed` arrives, cleared
    /// on decision.
    pub awaiting_hitl: bool,
```
In `StepCard::new`, add `awaiting_hitl: false,` to the struct literal.

- [ ] **Step 4: Add the App methods** (in `impl App`, near `update_step_completed`)

```rust
    /// Flag the card with this `step_id` as awaiting a HITL decision.
    pub fn mark_card_awaiting(&mut self, step_id: &str) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.awaiting_hitl = true;
        }
    }

    /// Clear the awaiting-HITL flag on the card with this `step_id`.
    pub fn clear_card_awaiting(&mut self, step_id: &str) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.awaiting_hitl = false;
        }
    }
```

- [ ] **Step 5: Wire the Hitl arm + decide path** (`mod.rs`)

In `handle_stream`'s `StreamMsg::Hitl { req, .. }` arm, after `app.hitl = Some(req);` — but you need the `step_id` before moving `req`. Rewrite the arm so it marks the card first:
```rust
        StreamMsg::Hitl { req, .. } => {
            app.saw_hitl_this_turn = true;
            if let Some(sid) = req.step_id.clone() {
                app.mark_card_awaiting(&sid);
            }
            let auto = app.auto_approve || app.session_tool_allow.contains(&req.tool_name);
            app.hitl = Some(req);
            if auto {
                decide_hitl_with_note(app, tx, true, true);
            }
        }
```
In `decide_hitl_with_note`, after `if let Some(req) = app.hitl.take() {`, clear the card flag:
```rust
    if let Some(req) = app.hitl.take() {
        if let Some(sid) = &req.step_id {
            app.clear_card_awaiting(sid);
        }
        let (h, a) = (app.home.clone(), app.agent.clone());
        // … rest unchanged …
```

- [ ] **Step 6: Run test + build**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::app::awaiting_tests && cargo check -p mur-core`
Expected: PASS + clean.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent/cli/step.rs mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): correlate HITL approval to its step card via step_id"
```

---

### Task 4: Render the inline approval row on the card

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/render_card.rs:17-94` (`card_lines` — append approval row when `awaiting_hitl`)
- Test: `mur-core/src/cmd/agent/cli/render_card.rs` (inline test)

**Interfaces:**
- Consumes: `StepCard.awaiting_hitl` (Task 3), `theme`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn awaiting_card_shows_inline_approval_row() {
        let mut c = StepCard::new("s1".into(), "edit".into(), serde_json::json!({"file_path":"a.rs"}));
        c.complete(true, "patched".into(), false, 1, None, 4);
        c.awaiting_hitl = true;
        let lines = card_lines(&c, theme::resolve_skin("dark"));
        let text: String = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>().join("\n");
        assert!(text.contains("[y]"));
        assert!(text.contains("approve"));
        assert!(text.contains("[n]"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::render_card::tests::awaiting_card_shows_inline_approval_row`
Expected: FAIL — no `[y]` in output.

- [ ] **Step 3: Append the approval row** in `card_lines`, just before the final `out`:

```rust
    // ── Inline HITL approval (P2) ────────────────────────────────────────────
    if card.awaiting_hitl {
        out.push(Line::from(vec![
            Span::styled(
                "  [y]",
                Style::default().fg(ratatui::style::Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" approve  ", Style::default().fg(theme.system)),
            Span::styled(
                "[a]",
                Style::default().fg(ratatui::style::Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" always  ", Style::default().fg(theme.system)),
            Span::styled(
                "[n]",
                Style::default().fg(ratatui::style::Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" deny / Esc", Style::default().fg(theme.system)),
        ]));
    }

    out
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::render_card`
Expected: PASS (all render_card tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/render_card.rs
git commit -m "feat(cli): inline [y]/[a]/[n] approval row on the awaiting step card"
```

---

### Task 5: Show the centered modal only as the old-runtime fallback

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/ui.rs:16-35` (`render` — gate `render_hitl` on absent `step_id`)
- Test: manual (the modal-vs-inline decision is a one-line guard; the inline row is tested in Task 4).

**Interfaces:**
- Consumes: `App.hitl` → `HitlRequest.step_id`.

- [ ] **Step 1: Gate the modal**

The current tail of `render` is:
```rust
    if let Some(hitl) = &app.hitl {
        render_hitl(f, hitl);
    }
```
Change it so the modal only shows when the approval is NOT correlated to a card (old runtime, no `step_id`) — otherwise the card renders the prompt inline:
```rust
    // Inline approval lives on the card (Task 4) when the runtime sent a
    // step_id. Fall back to the centered modal only for older runtimes.
    if let Some(hitl) = &app.hitl
        && hitl.step_id.is_none()
    {
        render_hitl(f, hitl);
    }
```

- [ ] **Step 2: Verify build + lint**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Expected: clean. (`render_hitl` is still used in the fallback path, so no dead-code warning.)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(cli): centered HITL modal becomes the old-runtime fallback; inline by default"
```

---

### Task 6: `cli/diff.rs` — edit args → diff lines

**Files:**
- Create: `mur-core/src/cmd/agent/cli/diff.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`mod diff;`)
- Modify: `mur-core/Cargo.toml` — confirm `diff = "0.1"` is present (it is, line 47, currently unused; this task makes it used)
- Test: `mur-core/src/cmd/agent/cli/diff.rs` (inline test)

**Interfaces:**
- Produces: `edit_diff_lines(name: &str, args: &serde_json::Value, theme: &'static Theme) -> Option<Vec<Line<'static>>>` — `Some(diff lines)` for an edit-like tool whose args contain a recognizable change, else `None`.
- Consumes: `diff` crate, `theme::Theme`.

- [ ] **Step 1: Write the failing test** (create the file test-first)

```rust
//! Render an edit-tool's `{file_path, old_string, new_string}` args as a
//! bounded -/+ diff, using the `diff` crate.

#[cfg(test)]
mod tests {
    use super::edit_diff_lines;
    use crate::cmd::agent::cli::theme;

    fn text(lines: &[ratatui::text::Line]) -> String {
        lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn edit_args_produce_minus_plus_lines() {
        let args = serde_json::json!({
            "file_path": "src/lib.rs", "old_string": "let x = 1;", "new_string": "let x = 2;"
        });
        let lines = edit_diff_lines("edit", &args, theme::resolve_skin("dark")).unwrap();
        let t = text(&lines);
        assert!(t.contains("src/lib.rs"));
        assert!(t.contains("- let x = 1;"));
        assert!(t.contains("+ let x = 2;"));
    }

    #[test]
    fn non_edit_tool_returns_none() {
        let args = serde_json::json!({ "command": "ls" });
        assert!(edit_diff_lines("bash", &args, theme::resolve_skin("dark")).is_none());
    }

    #[test]
    fn write_tool_renders_content_as_added() {
        let args = serde_json::json!({ "file_path": "new.rs", "content": "fn main() {}" });
        let lines = edit_diff_lines("write", &args, theme::resolve_skin("dark")).unwrap();
        assert!(text(&lines).contains("+ fn main() {}"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

First add `mod diff;` to `mod.rs`, then:
Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::diff`
Expected: FAIL — module/function missing.

- [ ] **Step 3: Implement `diff.rs`**

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

use super::theme::Theme;

/// Max diff lines shown inside a card body before truncating.
const DIFF_MAX_LINES: usize = 40;

/// Tool names (case-insensitive) whose args describe a file edit.
fn is_edit_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "edit" | "write" | "multiedit" | "str_replace" | "str_replace_editor"
    )
}

fn str_field<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| args.get(*k).and_then(|v| v.as_str()))
}

/// Build bounded -/+ diff lines for an edit-like tool, or `None` if this isn't
/// an edit we can render.
pub fn edit_diff_lines(
    name: &str,
    args: &serde_json::Value,
    theme: &'static Theme,
) -> Option<Vec<Line<'static>>> {
    if !is_edit_tool(name) {
        return None;
    }
    let path = str_field(args, &["file_path", "path"]).unwrap_or("");
    let old = str_field(args, &["old_string", "old_str"]);
    let new = str_field(args, &["new_string", "new_str", "content"]);
    // Need at least a `new` side to render anything.
    let new = new?;

    let mut out: Vec<Line<'static>> = Vec::new();
    if !path.is_empty() {
        out.push(Line::styled(
            format!("  {path}"),
            Style::default().fg(theme.system).add_modifier(Modifier::BOLD),
        ));
    }

    let mut pushed = 0usize;
    let mut push = |s: String, color: Color, dim: bool, out: &mut Vec<Line<'static>>| {
        if pushed < DIFF_MAX_LINES {
            let mut st = Style::default().fg(color);
            if dim {
                st = st.add_modifier(Modifier::DIM);
            }
            out.push(Line::styled(s, st));
        }
        pushed += 1;
    };

    match old {
        // Replacement: real line diff old→new.
        Some(old) => {
            for d in diff::lines(old, new) {
                match d {
                    diff::Result::Left(l) => push(format!("  - {l}"), Color::Red, false, &mut out),
                    diff::Result::Right(r) => push(format!("  + {r}"), Color::Green, false, &mut out),
                    diff::Result::Both(l, _) => push(format!("    {l}"), theme.system, true, &mut out),
                }
            }
        }
        // Create/overwrite: all of `new` is added.
        None => {
            for l in new.lines() {
                push(format!("  + {l}"), Color::Green, false, &mut out);
            }
        }
    }
    if pushed > DIFF_MAX_LINES {
        out.push(Line::styled(
            format!("  … +{} more diff line(s)", pushed - DIFF_MAX_LINES),
            Style::default().fg(theme.system).add_modifier(Modifier::DIM),
        ));
    }
    Some(out)
}
```

> Note: `theme.system` is a `Color` (see other render code). If `Line::styled`'s closure capture of `pushed` trips the borrow checker, hoist `push` into a plain `fn` taking `&mut usize` — keep behavior identical.

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::diff`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/diff.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): edit-args → bounded -/+ diff lines (reuses the diff crate)"
```

---

### Task 7: Render the diff in edit-tool cards

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/render_card.rs:17-94` (`card_lines` — render diff instead of raw args for edit tools)
- Test: `mur-core/src/cmd/agent/cli/render_card.rs` (inline test)

**Interfaces:**
- Consumes: `diff::edit_diff_lines` (Task 6).

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn edit_card_renders_diff_not_raw_json() {
        let c = StepCard::new(
            "s1".into(), "edit".into(),
            serde_json::json!({"file_path":"a.rs","old_string":"old","new_string":"new"}),
        );
        let lines = card_lines(&c, theme::resolve_skin("dark"));
        let text: String = lines.iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>().join("\n");
        assert!(text.contains("- old"));
        assert!(text.contains("+ new"));
        // raw JSON key should NOT be shown for an edit card
        assert!(!text.contains("\"old_string\""));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::render_card::tests::edit_card_renders_diff_not_raw_json`
Expected: FAIL — shows raw JSON, not the diff.

- [ ] **Step 3: Branch the args section** in `card_lines`. Replace the `// ── Args (bounded) ──` block's opening so an edit tool renders a diff instead of JSON:

```rust
    // ── Args: diff for edit tools, else bounded JSON ─────────────────────────
    if let Some(diff_lines) = super::diff::edit_diff_lines(&card.name, &card.args, theme) {
        out.extend(diff_lines);
    } else if !card.args.is_null() {
        let pretty = serde_json::to_string_pretty(&card.args).unwrap_or_default();
        let total_lines = pretty.lines().count();
        for l in pretty.lines().take(ARGS_MAX_LINES) {
            out.push(Line::styled(
                format!(" {l}"),
                Style::default().fg(theme.system),
            ));
        }
        if total_lines > ARGS_MAX_LINES {
            out.push(Line::styled(
                format!(" … +{} more", total_lines - ARGS_MAX_LINES),
                Style::default().fg(theme.system).add_modifier(Modifier::DIM),
            ));
        }
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::render_card`
Expected: PASS (all render_card tests, incl. the new one and the existing ones).

- [ ] **Step 5: Final P2a verification + commit**

Run:
```bash
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli
cargo clippy -p mur-core -p mur-agent-runtime -- -D warnings
cargo fmt --check
```
Expected: all green.

```bash
git add mur-core/src/cmd/agent/cli/render_card.rs
git commit -m "feat(cli): edit-tool cards render an inline diff instead of raw JSON args"
```

---

## Manual verification (after all tasks)

1. Build: `cargo build --release -p mur-core -p mur-agent-runtime`.
2. Restart a tool-using agent onto the new runtime (repoint `~/.local/bin/mur_agent_<name>` → `target/release/mur-agent-runtime`, then `mur agent restart <name>`).
3. `./target/release/mur agent cli <name>`; ask it to **edit a file** (e.g. "change the version in Cargo.toml to 9.9.9"). Confirm:
   - the edit card shows a `- old` / `+ new` **diff** (not raw JSON);
   - the **inline `[y]/[a]/[n]`** row appears on that card (no centered modal);
   - pressing `y` approves and the turn continues; pressing `n` denies.
4. Point the cli at an **old** runtime (no `step_id` in approval) and confirm the **centered modal** still appears (fallback).

## Out of scope (P2b — separate plan)

- Risk-tiered approval *lanes* (needs a runtime `resolve_tool_rule` to surface `ToolRule.risk` in the approval).
- Recently-denied list + `r`-to-retry.
- Standalone `/diff` per-turn navigable viewer.
- `Ctrl+O` scrollback dump to `$EDITOR`; notify-on-blur.

## Self-Review (completed)

- **Spec coverage:** D4 inline HITL (T1–T5), §E diff viewer inline-on-card (T6–T7). Risk lanes / recently-denied / `/diff` view / scrollback / notify → explicitly P2b. ✔
- **Placeholder scan:** none — every step has runnable code/commands. ✔
- **Type consistency:** `step_id`/`awaiting_hitl`/`mark_card_awaiting`/`clear_card_awaiting`/`edit_diff_lines` names match across tasks; `HitlRequest.step_id: Option<String>` consumed in T3/T5 as defined in T2. ✔
