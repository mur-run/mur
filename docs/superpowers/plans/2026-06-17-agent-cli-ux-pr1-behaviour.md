# Agent CLI UX — PR 1: Keyboard & Mouse Behaviour

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add mouse-wheel scroll, double-ESC to clear input, and double-ESC to cancel-and-restore a submitted message while the agent is streaming.

**Architecture:** Pure state machine (`esc_action()`) extracted for testability. New fields on `App` track ESC timing and last sent text. Mouse capture enabled via crossterm. Status bar updated to show ESC hint and scroll position.

**Tech Stack:** Rust, ratatui 0.29, crossterm 0.28, tui-textarea 0.7, tokio

**Spec:** `docs/superpowers/specs/2026-06-17-agent-cli-ux-improvements-design.md` — PR 1 section

---

## File Map

| File | Change |
|------|--------|
| `mur-core/src/cmd/agent/cli/app.rs` | Add `EscAction` enum, `esc_action()` fn, `ESC_DOUBLE_WINDOW` const, new `App` fields, update `start_new_session()` |
| `mur-core/src/cmd/agent/cli/mod.rs` | Add `MOUSE_SCROLL_STEP`, `EnableMouseCapture`/`DisableMouseCapture`, ESC handler, `Event::Mouse` arm |
| `mur-core/src/cmd/agent/cli/ui.rs` | Update `render_status()` for scroll indicator and ESC hint |

---

### Task 1: Add EscAction enum and esc_action() pure function

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs`

- [ ] **Step 1: Add ESC_DOUBLE_WINDOW constant and EscAction enum**

Open `app.rs`. After the `SLASH_COMMANDS` const (around line 116), add:

```rust
pub const ESC_DOUBLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

/// Result of pressing Escape — computed from current app state without mutation.
#[derive(Debug, PartialEq, Eq)]
pub enum EscAction {
    /// First ESC (or window expired) — arm the double-ESC window.
    Arm,
    /// Second ESC while not streaming and input non-empty — clear the input box.
    ClearInput,
    /// Second ESC while streaming — cancel in-flight turn and restore last sent text.
    CancelAndRestore,
    /// Nothing to act on (e.g. double-ESC but nothing to cancel or clear).
    Nothing,
}

/// Pure double-ESC state machine. Extracted so it can be unit-tested without
/// wall-clock dependency.
pub fn esc_action(
    last_esc_at: Option<std::time::Instant>,
    streaming: bool,
    input_empty: bool,
) -> EscAction {
    if let Some(t) = last_esc_at {
        if t.elapsed() < ESC_DOUBLE_WINDOW {
            // Second press within window
            return if streaming {
                EscAction::CancelAndRestore
            } else if !input_empty {
                EscAction::ClearInput
            } else {
                EscAction::Nothing
            };
        }
    }
    // First press (or window expired): arm if there is something to act on
    if streaming || !input_empty {
        EscAction::Arm
    } else {
        EscAction::Nothing
    }
}
```

- [ ] **Step 2: Write unit tests for esc_action()**

Inside the `#[cfg(test)]` block at the bottom of `app.rs`, add after the last existing test:

```rust
    // ── ESC state machine ────────────────────────────────────────────────────

    #[test]
    fn esc_arm_when_streaming_and_empty_input() {
        assert_eq!(esc_action(None, true, true), EscAction::Arm);
    }

    #[test]
    fn esc_arm_when_not_streaming_and_has_text() {
        assert_eq!(esc_action(None, false, false), EscAction::Arm);
    }

    #[test]
    fn esc_nothing_when_not_streaming_and_empty() {
        assert_eq!(esc_action(None, false, true), EscAction::Nothing);
    }

    #[test]
    fn esc_cancel_restore_on_second_press_while_streaming() {
        let t = std::time::Instant::now() - std::time::Duration::from_millis(100);
        assert_eq!(esc_action(Some(t), true, true), EscAction::CancelAndRestore);
    }

    #[test]
    fn esc_cancel_restore_on_second_press_streaming_has_text() {
        let t = std::time::Instant::now() - std::time::Duration::from_millis(100);
        assert_eq!(esc_action(Some(t), true, false), EscAction::CancelAndRestore);
    }

    #[test]
    fn esc_clear_on_second_press_not_streaming_has_text() {
        let t = std::time::Instant::now() - std::time::Duration::from_millis(100);
        assert_eq!(esc_action(Some(t), false, false), EscAction::ClearInput);
    }

    #[test]
    fn esc_nothing_on_second_press_not_streaming_empty() {
        let t = std::time::Instant::now() - std::time::Duration::from_millis(100);
        assert_eq!(esc_action(Some(t), false, true), EscAction::Nothing);
    }

    #[test]
    fn esc_expired_window_rearms() {
        // 500ms > ESC_DOUBLE_WINDOW (400ms) → treated as first press, not second
        let t = std::time::Instant::now() - std::time::Duration::from_millis(500);
        assert_eq!(esc_action(Some(t), true, true), EscAction::Arm);
    }
```

- [ ] **Step 3: Run tests — expect PASS**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo nextest run -p mur-core esc_ 2>&1
```

Expected: 8 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(agent-cli): add EscAction enum + esc_action() pure function"
```

---

### Task 2: Add ESC-related fields to App and update lifecycle methods

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs`

- [ ] **Step 1: Add three new fields to the App struct**

Inside `pub struct App { ... }`, add these three fields after `cwd_sent: bool`:

```rust
    /// When the user last pressed ESC; used to detect a double-ESC within
    /// ESC_DOUBLE_WINDOW. `None` means no pending ESC.
    pub last_esc_at: Option<std::time::Instant>,
    /// True while we want the status bar to show an ESC hint.
    pub esc_hint: bool,
    /// The most recently submitted (non-slash, non-shell) chat message.
    /// Double-ESC while streaming restores this into the input box.
    pub last_sent: Option<String>,
```

- [ ] **Step 2: Initialize new fields in App::new()**

Inside `App::new()`, in the `Self { ... }` initializer, add after `cwd_sent: false`:

```rust
            last_esc_at: None,
            esc_hint: false,
            last_sent: None,
```

- [ ] **Step 3: Clear ESC state and last_sent in start_new_session()**

In `start_new_session()`, add these three lines after `self.cwd_sent = false;`:

```rust
        self.last_sent = None;
        self.last_esc_at = None;
        self.esc_hint = false;
```

- [ ] **Step 4: Write a test for start_new_session clearing last_sent**

In the `#[cfg(test)]` block, add:

```rust
    #[test]
    fn start_new_session_clears_last_sent() {
        let home = tempdir().unwrap();
        let mut a = app_at(&home);
        a.last_sent = Some("hello".into());
        a.last_esc_at = Some(std::time::Instant::now());
        a.esc_hint = true;
        let s = Session::create(home.path(), "a").unwrap();
        a.start_new_session(s);
        assert!(a.last_sent.is_none());
        assert!(a.last_esc_at.is_none());
        assert!(!a.esc_hint);
    }
```

- [ ] **Step 5: Run tests — expect PASS**

```bash
cargo nextest run -p mur-core start_new_session 2>&1
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(agent-cli): add last_esc_at/esc_hint/last_sent fields to App"
```

---

### Task 3: Capture last_sent in submit()

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

The regular-chat path in `submit()` is around line 355–390. The code currently reads:

```rust
    app.clear_input();             // line 361

    let task_id = app.begin_user_turn(&trimmed);  // line 363
```

- [ ] **Step 1: Add last_sent assignment before clear_input**

Change those two lines to:

```rust
    app.last_sent = Some(trimmed.clone());
    app.clear_input();

    let task_id = app.begin_user_turn(&trimmed);
```

(No other changes to `submit()`.)

- [ ] **Step 2: Write a test for last_sent being set**

There's no direct test for `submit()` (it's async and uses mpsc). Instead, add an `App`-level helper test in `app.rs` — the logic is that `last_sent` is set before `clear_input`. We can verify this manually with the TUI once the handler is wired.

Skip automated test here; rely on manual verification in Task 5.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): capture last_sent before clearing input on submit"
```

---

### Task 4: Wire ESC handler in handle_event

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

- [ ] **Step 1: Add EscAction to the use statement**

At the top of `mod.rs`, line 36:

```rust
use self::app::{App, EscAction, Role, SlashCmd, esc_action, parse_slash};
```

(Add `EscAction` and `esc_action` to the existing import.)

- [ ] **Step 2: Add non-ESC key clears ESC state**

Inside `handle_event`, the `Event::Key` arm contains a `match key.code { ... }` block. Add these lines as the FIRST statement inside the `Event::Key` arm, before the `match key.code`:

```rust
        // Any key other than ESC cancels a pending double-ESC window.
        if key.code != KeyCode::Esc {
            app.last_esc_at = None;
            app.esc_hint = false;
        }
```

- [ ] **Step 3: Add ESC arm to the key code match**

Inside `match key.code { ... }`, add the `KeyCode::Esc` arm BEFORE the `_ => { app.input.input(key); }` catch-all:

```rust
            KeyCode::Esc => {
                let action = esc_action(
                    app.last_esc_at,
                    app.streaming,
                    app.input_text().is_empty(),
                );
                match action {
                    EscAction::Arm => {
                        app.last_esc_at = Some(std::time::Instant::now());
                        app.esc_hint = true;
                    }
                    EscAction::ClearInput => {
                        app.clear_input();
                        app.last_esc_at = None;
                        app.esc_hint = false;
                    }
                    EscAction::CancelAndRestore => {
                        cancel_in_flight(app, tx);
                        if let Some(text) = app.last_sent.clone() {
                            app.set_input(&text);
                        }
                        app.push_system("cancelled — message restored");
                        app.last_esc_at = None;
                        app.esc_hint = false;
                    }
                    EscAction::Nothing => {
                        app.last_esc_at = None;
                        app.esc_hint = false;
                    }
                }
            }
```

- [ ] **Step 4: Verify it compiles**

```bash
cargo build -p mur-core 2>&1
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): wire double-ESC clear + cancel-restore handler"
```

---

### Task 5: Add mouse scroll support

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

- [ ] **Step 1: Add mouse events to crossterm imports**

Line 22–25 currently:

```rust
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers,
};
```

Change to:

```rust
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, EnableMouseCapture, DisableMouseCapture,
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
```

- [ ] **Step 2: Add MOUSE_SCROLL_STEP constant**

Find the existing `const SCROLL_STEP: u16 = 5;` (around line 44). Add the new constant directly after it:

```rust
const SCROLL_STEP: u16 = 5;         // PageUp / PageDown
const MOUSE_SCROLL_STEP: u16 = 1;   // mouse wheel / trackpad (fires 10–20×/sec on trackpad)
```

- [ ] **Step 3: Enable mouse capture in TerminalGuard::enter()**

The current `enter()` body:

```rust
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste)
            .context("enter alternate screen")?;
        Ok(Self)
    }
```

Change the `execute!` line to:

```rust
        execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)
            .context("enter alternate screen")?;
```

- [ ] **Step 4: Disable mouse capture in TerminalGuard::drop()**

The current `drop()` body:

```rust
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            cursor::Show
        );
        let _ = disable_raw_mode();
    }
```

Change to:

```rust
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture,
            cursor::Show
        );
        let _ = disable_raw_mode();
    }
```

- [ ] **Step 5: Add Event::Mouse arm in handle_event**

In `handle_event`, there is currently an `Event::Paste(text)` arm and a final `_ => {}` catch-all. Add the `Event::Mouse` arm after `Event::Paste`:

```rust
        Event::Mouse(mouse_ev) => match mouse_ev.kind {
            MouseEventKind::ScrollUp => {
                app.scroll_back = app.scroll_back.saturating_add(MOUSE_SCROLL_STEP);
            }
            MouseEventKind::ScrollDown => {
                app.scroll_back = app.scroll_back.saturating_sub(MOUSE_SCROLL_STEP);
            }
            _ => {}
        },
```

- [ ] **Step 6: Build**

```bash
cargo build -p mur-core 2>&1
```

Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): mouse wheel scroll support (MOUSE_SCROLL_STEP=1)"
```

---

### Task 6: Update status bar — scroll indicator and ESC hint

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/ui.rs`

- [ ] **Step 1: Update render_status() to show scroll and ESC hints**

The `render_status` function (line 139–183). Replace the body entirely with:

```rust
fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let (msg, color) = if app.hitl.is_some() {
        (
            "tool approval needed — [y] approve · [a] always (session) · [n] deny".to_string(),
            Color::Yellow,
        )
    } else if app.streaming {
        let spin = SPINNER[app.spinner % SPINNER.len()];
        (format!("{spin} generating… Ctrl+C to cancel"), AGENT)
    } else {
        let ctx = if app.context_task_id.is_some() {
            " · context kept"
        } else {
            ""
        };
        (format!("ready{ctx}"), SYSTEM)
    };

    // Right-side hint: scroll indicator takes priority over ESC hint.
    let right_hint: Option<(String, Color)> = if app.scroll_back > 0 {
        Some((
            format!("↑ {} lines · ⬇ to bottom", app.scroll_back),
            SYSTEM,
        ))
    } else if app.esc_hint {
        let hint = if app.streaming {
            "ESC again to cancel"
        } else {
            "ESC again to clear"
        };
        Some((hint.to_string(), SYSTEM))
    } else {
        None
    };

    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.agent),
            Style::default().fg(Color::Black).bg(AGENT),
        ),
        Span::raw("  "),
    ];
    if app.auto_approve {
        spans.push(Span::styled(
            " AUTO ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    if let Some(meta) = &app.channel {
        let short: String = meta.id.chars().take(8).collect();
        spans.push(Span::styled(
            format!(" ⏵ {}:{} ", short, meta.state),
            Style::default().fg(Color::Cyan),
        ));
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled(msg, Style::default().fg(color)));
    if let Some((hint, hint_color)) = right_hint {
        spans.push(Span::styled(
            format!("  ·  {hint}"),
            Style::default().fg(hint_color).add_modifier(Modifier::DIM),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

- [ ] **Step 2: Build**

```bash
cargo build -p mur-core 2>&1
```

Expected: no errors.

- [ ] **Step 3: Run full test suite**

```bash
cargo nextest run -p mur-core 2>&1
```

Expected: all tests pass (the 2 `summarize::rollup` week tests may fail spuriously — unrelated, known issue).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(agent-cli): scroll indicator + ESC hint in status bar"
```

---

### Task 7: Manual verification

- [ ] **Step 1: Build and run**

```bash
cargo build -p mur-core
mur agent cli <any-running-agent>
```

- [ ] **Step 2: Test mouse scroll**

Scroll up in the chat pane with the trackpad or mouse wheel. Expected: transcript scrolls up 1 line per scroll event. Scrolling down returns to bottom. Status bar shows `↑ N lines · ⬇ to bottom` while scrolled.

- [ ] **Step 3: Test double-ESC clear (not streaming)**

Type some text in the input box. Press ESC once: status bar should show `ESC again to clear`. Press ESC again within 400ms: input clears.

- [ ] **Step 4: Test double-ESC cancel-restore (streaming)**

Send a message. While the agent is generating: press ESC once (status bar shows `ESC again to cancel`), press ESC again: agent stream cancels, your original message reappears in the input box. Status bar shows `cancelled — message restored` system notice.

- [ ] **Step 5: Test double-ESC window expiry**

Press ESC, wait 500ms, press ESC again: should NOT cancel/clear (window expired, second press is treated as first press of a new window).

- [ ] **Step 6: Final commit if any fixups needed**

```bash
git add -p
git commit -m "fix(agent-cli): <describe any fixup>"
```
