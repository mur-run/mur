# mur agent cli — Notify-on-Blur — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a turn finishes (or needs HITL approval, or fails) while the terminal is **unfocused**, fire an OS notification — so you can start a long turn, tab away, and get pinged only when you're not watching.

**Architecture:** Enable crossterm focus reporting in `TerminalGuard`; track `App.focused` from `Event::FocusGained`/`Event::FocusLost` in `handle_event`; in `handle_stream`, when an attention-worthy event arrives (`Done`/`Hitl`/`Err`) and `!app.focused`, call a best-effort `notify_unfocused` helper (mirrors the existing `cmd/project.rs::send_notification`: macOS `osascript`, Linux `notify-send`, spawned and ignored). Notify ONLY when unfocused — no spam while watching.

**Tech Stack:** Rust (edition 2024), crossterm 0.28 (focus events — `EnableFocusChange`/`Event::FocusGained`), `std::process::Command` (osascript/notify-send).

## Global Constraints

- **Independent cli feature** — branch from `main` (this plan was written on `feat/agent-cli-notify-on-blur`, already cut off `main e6ca826b`). Works on any cli version; orthogonal to the unmerged Glass Box stack.
- **Rust edition 2024**; no hardcoded values; brand "MUR" uppercase in user-facing copy (the notification title uses the agent name).
- **Tests:** mur-core needs `ORT_STRATEGY=download`; toolchain cargo if rustup broken (`export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"`, plain `cargo test`).
- **Lint gate:** `cargo clippy -p mur-core -- -D warnings` + `cargo fmt`.
- **No in-terminal bell while the TUI is up** — the cli is a full-screen alternate-screen app; writing `\x07`/`eprintln!` to std streams would corrupt the display. Notify out-of-band only (osascript/notify-send).
- **Best-effort:** notifications never block or error the loop — spawn and ignore the result (`let _ = …spawn();`), exactly like `send_notification`.

---

### Task 1: Track terminal focus

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (imports; `TerminalGuard::enter`/`Drop`; `handle_event` focus arms)
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`App.focused` field + ctor init)
- Test: `mur-core/src/cmd/agent/cli/mod.rs` (a focused-event arm test if an `App` is cheaply constructible; else build-only — see Step 5)

**Interfaces:**
- Produces: `App.focused: bool` (default `true`); set by `Event::FocusGained`/`Event::FocusLost` in `handle_event`.

- [ ] **Step 1: Add the `focused` field** (`app.rs`)

In the `App` struct, after `mascot_mode`:
```rust
    /// True while the terminal is focused. Driven by crossterm focus events;
    /// used to suppress notifications while the user is watching.
    pub focused: bool,
```
In `App::new`, after the `mascot_mode:` init:
```rust
            // Assume focused at startup; crossterm corrects it on the first
            // FocusLost. (Terminals that don't report focus stay `true` → no
            // notifications, which is the safe default.)
            focused: true,
```

- [ ] **Step 2: Enable focus reporting in `TerminalGuard`** (`mod.rs`)

Add `EnableFocusChange, DisableFocusChange` to the crossterm event import (the `use crossterm::event::{…}` block):
```rust
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseEventKind,
};
```
In `TerminalGuard::enter`'s `execute!`, add `EnableFocusChange`:
```rust
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            EnableFocusChange
        )
```
In `Drop`'s `execute!`, add `DisableFocusChange` (before `cursor::Show`):
```rust
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture,
            DisableFocusChange,
            cursor::Show
        );
```
> Also mirror this in the panic hook if it has its own `execute!` restore block (grep `set_hook` / `LeaveAlternateScreen` in `run_tui`) — add `DisableFocusChange` there too for symmetry. If the panic hook reuses the same teardown, no change needed.

- [ ] **Step 3: Handle the focus events** (`mod.rs handle_event`)

In `handle_event`'s top-level `match ev { … }`, add two arms right before the final `_ => {}` catch-all (after the `Event::Mouse(...)` arm):
```rust
        Event::FocusGained => app.focused = true,
        Event::FocusLost => app.focused = false,
```

- [ ] **Step 4: Build + lint**

Run: `ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Expected: clean.

- [ ] **Step 5: Focus-arm test (if cheap) + commit**

If `app.rs` has a test constructor (grep `#[cfg(test)]`/`fn test_` in `app.rs` / `App::new` usage in tests), add a test driving the focus events through `handle_event`:
```rust
    #[tokio::test]
    async fn focus_events_toggle_focused() {
        let mut a = /* App built the way the file's other tests build one */;
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        handle_event(&mut a, crossterm::event::Event::FocusLost, &tx).await;
        assert!(!a.focused);
        handle_event(&mut a, crossterm::event::Event::FocusGained, &tx).await;
        assert!(a.focused);
    }
```
> If constructing an `App` in a test is heavy on `main` (no `test_fixture`; `App::new` needs a `Session` + `&'static Theme`), do NOT build fixture scaffolding just for this — skip the test and rely on the build + manual verification (the arms are two trivial assignments). Note the skip in the commit body.

```bash
git add mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(cli): track terminal focus via crossterm focus events"
```

---

### Task 2: Notify when an attention event lands unfocused

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (a `notify_unfocused` helper + a pure testable `notify_script`; trigger in `handle_stream` `Done`/`Hitl`/`Err`)
- Test: `mur-core/src/cmd/agent/cli/mod.rs` (inline — `notify_script` escaping)

**Interfaces:**
- Produces: `notify_unfocused(title: &str, message: &str)` (best-effort OS notification); `notify_script(title, message) -> String` (the escaped macOS osascript line — pure, testable).
- Consumes: `App.focused` (Task 1).

- [ ] **Step 1: Write the failing test** (the testable seam is the osascript-arg escaping)

```rust
#[cfg(test)]
mod notify_tests {
    use super::notify_script;

    #[test]
    fn notify_script_escapes_quotes() {
        let s = notify_script("rustsmith", r#"finished "the" task"#);
        assert!(s.contains("display notification"));
        assert!(s.contains(r#"with title \"rustsmith\""#));
        // embedded quotes in the message are escaped, not left raw
        assert!(s.contains(r#"finished \"the\" task"#));
        assert!(!s.contains(r#"finished "the""#)); // no unescaped inner quote
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::notify 2>&1 | tail -20`
Expected: FAIL — `notify_script` not found.

- [ ] **Step 3: Implement the helpers** (mirror `cmd/project.rs::send_notification`)

```rust
/// The macOS `osascript` line for a notification, with quotes escaped. Pure so
/// the escaping is unit-tested; the spawn is in `notify_unfocused`.
fn notify_script(title: &str, message: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        message.replace('\\', "\\\\").replace('"', "\\\""),
        title.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// Best-effort OS notification — fired only when the terminal is unfocused.
/// Never blocks or errors the event loop (spawn-and-ignore), and emits no
/// in-terminal bell (the TUI owns the alternate screen).
fn notify_unfocused(title: &str, message: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("osascript")
            .args(["-e", &notify_script(title, message)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args([title, message])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, message); // no-op on other platforms
    }
}
```
> `notify_script` is referenced inside the `#[cfg(target_os = "macos")]` block; on Linux/other it's unused → add `#[cfg_attr(not(target_os = "macos"), allow(dead_code))]` on `notify_script` (the test still uses it under `#[cfg(test)]`, so it's not dead in test builds, but a non-macOS release build would warn). Confirm clippy `-D warnings` is clean on this host (macOS — `notify_script` is used) AND keep the allow so Linux CI stays clean.

- [ ] **Step 4: Trigger it in `handle_stream`** (the attention moments, only when `!app.focused`)

In `handle_stream`'s `match msg`, gate notifications on `!app.focused`:
```rust
        StreamMsg::Hitl { req, .. } => {
            let auto = app.auto_approve || app.session_tool_allow.contains(&req.tool_name);
            if !app.focused && !auto {
                notify_unfocused(&app.agent, &format!("Tool approval needed: {}", req.tool_name));
            }
            app.hitl = Some(req);
            if auto {
                decide_hitl_with_note(app, tx, true, true);
            }
        }
        StreamMsg::Done { task, .. } => {
            if !app.focused {
                notify_unfocused(&app.agent, "Turn finished");
            }
            match stream::task_outcome(&task) {
                Ok((reply, task_id)) => app.finish_agent_turn(reply, task_id),
                Err(cause) => app.fail_turn(&cause),
            }
        }
        StreamMsg::Err { error, .. } => {
            if !app.focused {
                notify_unfocused(&app.agent, "Turn failed");
            }
            app.fail_turn(&error);
        }
```
> Note: the `Hitl` notify fires before the auto-approve path so an *auto-approved* tool (no user action needed) does NOT notify — only a gate that actually needs the user. Keep the rest of each arm's logic byte-identical to the original.

- [ ] **Step 5: Run test + build**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli::notify && cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt`
Expected: PASS + clean. Confirm existing cli tests still pass: `ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): notify on turn finish/approval/failure when the terminal is unfocused"
```

---

## Manual verification (after both tasks)

1. Build: `cargo build --release -p mur-core`.
2. `./target/release/mur agent cli <agent>`; send a message that takes a few seconds.
3. **Click away to another window** while it runs. Confirm a macOS notification ("<agent> · Turn finished") appears when the turn completes — and NOT when you're focused on the terminal.
4. With a gated tool (an `ask`-policy agent), tab away during a turn → confirm a "Tool approval needed: <tool>" notification.
5. Focused the whole time → no notifications (no spam).

## Out of scope

- In-app notification preferences / a `/notify off` toggle (env or flag could disable it later).
- Notification on every tool step (too noisy — only turn-end / approval / failure).
- The companion's richer notification channels (this is the lightweight cli path).

## Self-Review (completed)

- **Spec coverage:** focus tracking (T1), unfocused-notify on Done/Hitl/Err (T2). ✔
- **Placeholder scan:** none — code in every step; the few "grep to confirm" notes are anchors against real code (panic-hook teardown, App test-constructor availability), not logic placeholders. ✔
- **Type consistency:** `App.focused` (T1) read in `handle_stream` (T2); `notify_unfocused`/`notify_script` defined + consumed in T2. ✔
