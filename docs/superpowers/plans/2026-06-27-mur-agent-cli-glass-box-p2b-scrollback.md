# mur agent cli — Glass Box P2b: Scrollback Escape Hatch — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Ctrl+O` suspends the full-screen TUI, prints the full plain-text transcript to native scrollback (so the user can select/copy/search) and saves it to a temp file, then returns to chat on Enter.

**Architecture:** A new `cli/dump.rs` turns `app.messages` into plain text (every message + tool card fully expanded, no styling). A `Ctrl+O` arm in `handle_event` writes that text to a temp file, leaves the alternate screen + disables mouse capture (so the terminal's native copy/scrollback work), prints the transcript + a footer, blocks on a canonical-mode `read_line` for Enter, then re-enters the alternate screen. The event loop auto-redraws on the next iteration.

**Tech Stack:** Rust (edition 2024), crossterm (`execute!`, alt-screen/mouse/raw-mode toggles — already imported in `mod.rs`), ratatui (transcript types). No new dependency.

## Global Constraints

- **Builds on P2a** — branch from `feat/agent-cli-glass-box-p2a` (PR #518, stacked on P1 #517). All cli types exist: `ChatMsg`, `StepCard`, `Role`.
- **Rust edition 2024**; no hardcoded values (named `const`); brand "MUR" uppercase in user-facing copy.
- **Single source file ≤ 800 lines** — `mod.rs` is large; the transcript builder goes in a new `cli/dump.rs`, only the small `Ctrl+O` arm + helper land in `mod.rs`.
- **Tests:** rustup proxy is often broken in agent sessions — if `cargo` is not on PATH use `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"` and plain `cargo test` (the `cargo-nextest` binary is absent). mur-core needs `ORT_STRATEGY=download`.
- **Lint gate:** `cargo clippy -p mur-core -- -D warnings` + `cargo fmt`.
- **Terminal lifecycle facts** (from the gather): `handle_event` does NOT hold the `Terminal`, but both it and the `Terminal` write to `io::stdout()`, so suspend/resume via direct `execute!(io::stdout(), …)` is safe. `TerminalGuard::enter` does `enable_raw_mode()` + `execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)`. The event loop calls `terminal.draw(...)` at the top of every iteration, so no manual redraw is needed after resume. Blocking `read_line` during the suspend is fine: the TUI is single-threaded and the streaming worker just queues mpsc messages until we return.

---

### Task 1: `cli/dump.rs` — plain-text transcript builder

**Files:**
- Create: `mur-core/src/cmd/agent/cli/dump.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`mod dump;`)
- Test: `mur-core/src/cmd/agent/cli/dump.rs` (inline test)

**Interfaces:**
- Produces: `transcript_to_text(messages: &[ChatMsg]) -> String` — the whole transcript as plain text; tool cards fully expanded (glyph + name + full pretty args + full output it has + error), reasoning included, no ratatui styling, no line caps.
- Consumes: `ChatMsg`, `Role` (app.rs), `StepCard`/`StepState` (step.rs).

- [ ] **Step 1: Write the failing test** (create the file test-first)

```rust
//! Render the visible transcript to plain text for the Ctrl+O scrollback dump.
//! Everything is fully expanded (no TUI line caps) and unstyled, so the user
//! can select/copy/search it natively.

#[cfg(test)]
mod tests {
    use super::transcript_to_text;
    use crate::cmd::agent::cli::app::{ChatMsg, Role};
    use crate::cmd::agent::cli::step::StepCard;

    #[test]
    fn renders_user_and_agent_and_reasoning() {
        let msgs = vec![
            ChatMsg::for_test(Role::User, "hello"),
            {
                let mut m = ChatMsg::for_test(Role::Agent, "hi there");
                m.thinking = "let me think".into();
                m
            },
        ];
        let t = transcript_to_text(&msgs);
        assert!(t.contains("you> hello"));
        assert!(t.contains("let me think"));   // reasoning kept in the dump
        assert!(t.contains("agent> hi there"));
    }

    #[test]
    fn renders_tool_card_fully_expanded() {
        let mut card = StepCard::new("s1".into(), "bash".into(), serde_json::json!({"command":"ls"}));
        card.complete(true, "a.rs\nb.rs".into(), false, 2, None, 5);
        let m = ChatMsg::tool_for_test(card);
        let t = transcript_to_text(&[m]);
        assert!(t.contains("bash"));
        assert!(t.contains("\"command\": \"ls\""));  // full args
        assert!(t.contains("a.rs"));                  // full output
        assert!(t.contains("b.rs"));
    }

    #[test]
    fn renders_error_card() {
        let mut card = StepCard::new("s1".into(), "bash".into(), serde_json::json!({}));
        card.complete(false, "boom".into(), false, 4, Some("exit 1".into()), 3);
        let t = transcript_to_text(&[ChatMsg::tool_for_test(card)]);
        assert!(t.contains("exit 1"));
    }
}
```

> `ChatMsg::new`/`tool` are private (`fn`, not `pub fn`). Add two `#[cfg(test)] pub` test constructors next to them in `app.rs` so the dump test can build messages:
> ```rust
> #[cfg(test)]
> impl ChatMsg {
>     pub fn for_test(role: Role, text: &str) -> Self { Self::new(role, text) }
>     pub fn tool_for_test(card: super::step::StepCard) -> Self { Self::tool(card) }
> }
> ```
> (Add this in `app.rs`, in the same module as `ChatMsg`.)

- [ ] **Step 2: Run test to verify it fails**

First add `mod dump;` to `mod.rs`, then:
Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::dump`
Expected: FAIL — `transcript_to_text` / `for_test` not found.

- [ ] **Step 3: Implement `dump.rs`**

```rust
use super::app::{ChatMsg, Role};
use super::step::StepCard;

/// The whole visible transcript as plain, unstyled text — tool cards fully
/// expanded — for the Ctrl+O scrollback dump.
pub fn transcript_to_text(messages: &[ChatMsg]) -> String {
    let mut out = String::new();
    for m in messages {
        if let Some(card) = &m.step {
            out.push_str(&card_text(card));
            continue;
        }
        match m.role {
            Role::User => {
                out.push_str("\nyou> ");
                out.push_str(&m.text);
                out.push('\n');
            }
            Role::Agent => {
                out.push('\n');
                if !m.thinking.is_empty() {
                    out.push_str("[reasoning]\n");
                    out.push_str(&m.thinking);
                    out.push('\n');
                }
                out.push_str("agent> ");
                out.push_str(&m.text);
                out.push('\n');
            }
            Role::System => {
                out.push_str("· ");
                out.push_str(&m.text);
                out.push('\n');
            }
            Role::Shell => {
                // already formatted as "$ cmd\noutput"
                out.push_str(&m.text);
                out.push('\n');
            }
        }
    }
    out
}

fn card_text(card: &StepCard) -> String {
    let mut s = String::new();
    let dur = card
        .duration_ms
        .map(|ms| format!(" · {ms}ms"))
        .unwrap_or_default();
    s.push_str(&format!("\n{} {}{}\n", card.glyph(), card.name, dur));
    if !card.args.is_null() {
        if let Ok(pretty) = serde_json::to_string_pretty(&card.args) {
            for l in pretty.lines() {
                s.push_str("  ");
                s.push_str(l);
                s.push('\n');
            }
        }
    }
    if let Some(err) = &card.error {
        s.push_str(&format!("  ✗ {err}\n"));
    }
    if !card.output.is_empty() {
        for l in card.output.lines() {
            s.push_str("  ");
            s.push_str(l);
            s.push('\n');
        }
        if card.truncated {
            s.push_str(&format!(
                "  … (output truncated to {} bytes; {} total)\n",
                card.output.len(),
                card.full_len
            ));
        }
    }
    s
}
```

- [ ] **Step 4: Register module + add the test constructors**

Add `mod dump;` to `cli/mod.rs` (alphabetical, near `mod diff;`). Add the `#[cfg(test)] impl ChatMsg { for_test, tool_for_test }` from Step 1's note to `app.rs`.

- [ ] **Step 5: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::dump`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/dump.rs mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(cli): plain-text transcript builder for the scrollback dump"
```

---

### Task 2: `Ctrl+O` scrollback escape hatch

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (a `Ctrl+O` arm in `handle_event` ~line 319 + a `scrollback_dump` helper)
- Modify: `mur-core/src/cmd/agent/cli/ui.rs` (optional one-line hint in the footer/help — see Step 4)
- Test: manual (terminal lifecycle can't be unit-tested; the transcript content is covered in Task 1)

**Interfaces:**
- Consumes: `dump::transcript_to_text` (Task 1); the crossterm imports already present in `mod.rs` (`execute!`, `EnterAlternateScreen`, `LeaveAlternateScreen`, `EnableMouseCapture`, `DisableMouseCapture`, `EnableBracketedPaste`, `DisableBracketedPaste`, `enable_raw_mode`, `disable_raw_mode`, `io`).

- [ ] **Step 1: Add the `scrollback_dump` helper** (in `mod.rs`, near `handle_event`)

```rust
/// Suspend the TUI, print the full transcript to native scrollback (so the
/// terminal's own select/copy/search work), also save it to a temp file, then
/// wait for Enter and resume. Blocking is intentional — the TUI is frozen while
/// the user reads/copies; streaming messages queue until we return.
fn scrollback_dump(app: &App) -> io::Result<()> {
    let text = super::dump::transcript_to_text(&app.messages);
    let path = std::env::temp_dir().join(format!("mur-transcript-{}.txt", app.agent));
    let _ = std::fs::write(&path, &text);

    // Suspend: leave alt-screen + restore the normal buffer where native copy
    // and scrollback work; drop raw mode so `read_line` is canonical.
    execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    disable_raw_mode()?;

    use std::io::Write;
    let mut out = io::stdout();
    writeln!(out, "{text}")?;
    writeln!(
        out,
        "\n─── full transcript · select/copy freely · saved to {} ───",
        path.display()
    )?;
    write!(out, "press Enter to return to chat… ")?;
    out.flush()?;
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);

    // Resume.
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    Ok(())
}
```

> Confirm these symbols are already imported at the top of `mod.rs` (they are — `TerminalGuard` uses all of them). If `App` isn't in scope by that name in this position, use the same path the surrounding fns use (`handle_event` takes `app: &mut App`, so `App` is in scope).

- [ ] **Step 2: Wire the `Ctrl+O` key arm** in `handle_event`'s non-HITL `match key.code`, alongside the existing `Char('v') if ctrl` arm:

```rust
                KeyCode::Char('o') if ctrl => {
                    if let Err(e) = scrollback_dump(app) {
                        app.push_system(format!("scrollback view failed: {e}"));
                    }
                }
```

- [ ] **Step 3: Build + lint**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Expected: clean. (No unit test — the transcript content is tested in Task 1; the suspend/resume is verified manually below.)

- [ ] **Step 4: Surface the binding** (small discoverability touch)

In `ui.rs`, the input-box title currently reads `message — Enter to send · Alt+Enter newline · Ctrl+V image · /help · Ctrl+D quit`. Add `· Ctrl+O transcript` to that title string (find it with `rg -n "Ctrl\+D quit" mur-core/src/cmd/agent/cli/ui.rs`). One-line copy change; keep within the existing format.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(cli): Ctrl+O dumps the full transcript to native scrollback + a temp file"
```

---

## Manual verification (after both tasks)

1. Build: `cargo build --release -p mur-core`.
2. `./target/release/mur agent cli <agent>`; have a short exchange (a message + a tool turn).
3. Press **Ctrl+O**. Confirm:
   - the alternate screen is left and the **full transcript** prints to the normal terminal buffer (tool cards expanded, reasoning shown);
   - the footer shows `saved to /…/mur-transcript-<agent>.txt`;
   - you can **select/copy** text with the mouse and the terminal's **native scrollback / search** work;
   - pressing **Enter** returns to the chat exactly where you left off (the loop redraws).
4. `cat` the saved temp file to confirm it matches.

## Out of scope (later)

- `$EDITOR` integration (the temp-file path is printed; opening it is the user's choice).
- A `v`/`[` two-key transcript mode (Claude-Code-style) — the single Ctrl+O is enough for P2b.
- notify-on-blur, risk-tiered lanes, mid-turn steering — separate increments.

## Self-Review (completed)

- **Spec coverage:** the scrollback escape hatch (design §D) — transcript builder (T1) + Ctrl+O suspend/print/resume (T2). ✔
- **Placeholder scan:** none — every step has runnable code/commands. ✔
- **Type consistency:** `transcript_to_text(&[ChatMsg]) -> String` defined in T1, consumed in T2's `scrollback_dump`; `ChatMsg::for_test`/`tool_for_test` used only in tests. ✔
