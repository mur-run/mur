# Agent CLI UX Improvements Design

**Date:** 2026-06-17
**Status:** Approved for implementation
**Scope:** `mur-core/src/cmd/agent/cli/`

## Summary

Two-PR series improving the `mur agent cli` TUI:

- **PR 1** — keyboard/input behaviour (mouse scroll, double-ESC clear, double-ESC cancel+restore)
- **PR 2** — skin system + UI polish (three themes, rounded layout, badge status bar)

---

## PR 1: Keyboard & Mouse Behaviour

### 1. Mouse Scroll

**Where:** `TerminalGuard` (enter/drop) and `handle_event` in `mod.rs`.

Enable mouse capture on TUI entry and release it on exit:

```rust
// TerminalGuard::enter()
execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;

// TerminalGuard::drop()
execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste, DisableMouseCapture, cursor::Show);
```

Add a mouse arm to `handle_event`:

```rust
Event::Mouse(mouse_ev) => match mouse_ev.kind {
    MouseEventKind::ScrollUp   => app.scroll_back = app.scroll_back.saturating_add(SCROLL_STEP),
    MouseEventKind::ScrollDown => app.scroll_back = app.scroll_back.saturating_sub(SCROLL_STEP),
    _ => {}
}
```

No other mouse events (clicks, drag) are handled.

### 2. Double-ESC to Clear Input

**New `App` field:**

```rust
last_esc_at: Option<std::time::Instant>,
```

**Constant:**

```rust
const ESC_DOUBLE_WINDOW: Duration = Duration::from_millis(400);
```

**ESC key handler in `handle_event`:**

```
on ESC press:
  if input is empty → do nothing
  if last_esc_at is Some(t) && t.elapsed() < ESC_DOUBLE_WINDOW:
    clear input
    last_esc_at = None
  else:
    last_esc_at = Some(Instant::now())
    (optional: show dim hint in status "ESC again to clear")

on any non-ESC key press:
  last_esc_at = None
```

Status bar hint is ephemeral — stored as `App.esc_hint: bool`, cleared on next keypress.

### 3. Double-ESC to Cancel-and-Restore (while streaming)

**New `App` field:**

```rust
last_sent: Option<String>,
```

Set in `submit()` immediately before `app.clear_input()`.

**Modified double-ESC second-press logic:**

```
on double-ESC second press:
  if app.streaming:
    cancel_in_flight(app, tx)           // same path as Ctrl+C
    if let Some(text) = app.last_sent:
      app.set_input(&text)              // restore message to input box
    app.push_system("cancelled — message restored")
  else:
    clear input                         // standard clear
```

`last_sent` is NOT cleared by `start_new_session` or `/clear` (a stale restore after session reset is harmless; user can just ESC again to clear).

---

## PR 2: Skin System + UI Polish

### Theme Data Model

New file: `mur-core/src/cmd/agent/cli/theme.rs`

```rust
pub struct Theme {
    pub user:           Color,   // user turn label + text
    pub agent:          Color,   // agent turn label
    pub system:         Color,   // system/hint messages
    pub border:         Color,   // transcript + input borders
    pub border_title:   Color,   // border title text
    pub status_bg:      Color,   // status bar background
    pub badge_fg:       Color,   // agent-name badge foreground
    pub badge_bg:       Color,   // agent-name badge background
    pub separator:      Color,   // inter-message separator line
}

pub const DARK: Theme = Theme { /* refined dark palette */ };
pub const LIGHT: Theme = Theme { /* light palette */ };
pub const MUR: Theme = Theme {
    user:         Color::Rgb(0xa7, 0x8b, 0xfa),  // violet
    agent:        Color::Rgb(0xfb, 0xbf, 0x24),  // amber
    system:       Color::Rgb(0x33, 0x33, 0x5a),
    border:       Color::Rgb(0x2a, 0x2a, 0x5a),  // indigo
    border_title: Color::Rgb(0x33, 0x33, 0x7a),
    status_bg:    Color::Rgb(0x09, 0x09, 0x1a),
    badge_fg:     Color::Rgb(0xfb, 0xbf, 0x24),
    badge_bg:     Color::Rgb(0x1a, 0x14, 0x0a),
    separator:    Color::Rgb(0x1e, 0x1e, 0x44),
};
```

`App` gains `theme: &'static Theme`. Resolved once at startup; `/skin` updates it live.

### Skin Loading (precedence chain)

1. `--skin <name>` CLI flag (highest)
2. `cli.skin` key in `~/.mur/config.yaml`
3. `"dark"` (default)

Helper:

```rust
fn resolve_skin(name: &str) -> &'static Theme {
    match name {
        "light" => &theme::LIGHT,
        "mur"   => &theme::MUR,
        _       => &theme::DARK,
    }
}
```

### `/skin` Slash Command

Added to `SlashCmd` enum:

```rust
/// `/skin [dark|light|mur]` — switch theme live and persist to config.
Skin(Option<String>),
```

Handler:
- With no arg: print current skin name.
- With a valid name: call `resolve_skin`, update `app.theme`, write `cli.skin` to `~/.mur/config.yaml`.
- With an unknown name: show error listing valid names.

### UI Layout Changes (all skins)

**Borders:** Switch from `Borders::ALL` to `Borders::ROUNDED` for both the transcript pane and the input box. Ratatui provides rounded Unicode corners (`╭ ╮ ╰ ╯`) via `BorderType::Rounded`.

**Message separators:** Remove the blank `Line::default()` spacer between messages. Replace with a single `Line::styled("─".repeat(width), dim_style)` where `width` is the inner pane width minus 2. This keeps the visual break without eating vertical space.

**Status bar:** Replace plain text with:
```
[badge: agent-name]  ▶ state  ·  channel-short  (right-align: context kept / not saved)
```
Badge rendered as `Span` with `bg(badge_bg).fg(badge_fg)` style.

**Input box title:** Shortened hint text: `message — Enter · Alt+Enter newline · /help`.

### File Size Check

`ui.rs` (255 lines) and `app.rs` (640 lines) stay under the 800-line limit after this work. Adding `theme.rs` (~80 lines) keeps the split clean. If `app.rs` grows, split `SlashCmd` + `parse_slash` into `slash.rs`.

---

## Testing

**PR 1:**
- Unit tests in `app.rs` for `last_esc_at` transitions (single ESC, double within window, double outside window, non-ESC clears flag).
- Unit test: double-ESC while streaming sets input to `last_sent`.
- Manual: mouse scroll in `mur agent cli` in iTerm2/Ghostty.

**PR 2:**
- Unit test: `resolve_skin` returns correct theme for each name and unknown fallback.
- Unit test: `/skin` slash command parses correctly.
- Manual: visual check of each skin with `mur agent cli --skin dark|light|mur`.
- Manual: `/skin mur` persists to `~/.mur/config.yaml` and survives restart.
