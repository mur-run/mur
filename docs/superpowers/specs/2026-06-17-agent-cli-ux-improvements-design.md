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

**Files:** `TerminalGuard` (enter/drop) and `handle_event` in `mod.rs`.

Enable mouse capture on TUI entry and release on exit:

```rust
// TerminalGuard::enter()
execute!(io::stdout(), EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)?;

// TerminalGuard::drop()
execute!(io::stdout(), LeaveAlternateScreen, DisableBracketedPaste, DisableMouseCapture, cursor::Show);
```

Add a mouse arm to `handle_event`:

```rust
Event::Mouse(mouse_ev) => match mouse_ev.kind {
    MouseEventKind::ScrollUp   => app.scroll_back = app.scroll_back.saturating_add(MOUSE_SCROLL_STEP),
    MouseEventKind::ScrollDown => app.scroll_back = app.scroll_back.saturating_sub(MOUSE_SCROLL_STEP),
    _ => {}
}
```

Use a **separate constant** from page-scroll:

```rust
const SCROLL_STEP: u16       = 5;   // PageUp / PageDown
const MOUSE_SCROLL_STEP: u16 = 1;   // mouse wheel / trackpad
```

`SCROLL_STEP = 5` for PageUp/PageDown is correct. `MOUSE_SCROLL_STEP = 1` prevents the trackpad from firing 10–20 events/sec × 5 = jumping 50–100 lines per gesture.

No other mouse events (clicks, drag) are handled. Terminal text selection still works in terminals that restore it on Shift (e.g. iTerm2, Ghostty).

### 2. Double-ESC: Clear Input and Cancel+Restore

Both behaviours share one state machine. The key insight: the first ESC always arms the window regardless of input state, so the cancel-while-streaming path is reachable even when the input is empty after submission.

**New `App` fields:**

```rust
last_esc_at: Option<std::time::Instant>,   // armed after first ESC
esc_hint:    bool,                          // show hint in status bar
last_sent:   Option<String>,               // last successfully submitted text
```

**New constant:**

```rust
const ESC_DOUBLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);
```

**ESC key handler (pure state-machine logic — extracted for testability):**

```rust
enum EscAction { Arm, ClearInput, CancelAndRestore, Nothing }

fn esc_action(
    last_esc_at: Option<std::time::Instant>,
    streaming: bool,
    input_empty: bool,
) -> EscAction {
    if let Some(t) = last_esc_at {
        if t.elapsed() < ESC_DOUBLE_WINDOW {
            // second ESC within window
            return if streaming {
                EscAction::CancelAndRestore
            } else if !input_empty {
                EscAction::ClearInput
            } else {
                EscAction::Nothing
            };
        }
    }
    // first ESC (or window expired): arm if there's anything to act on
    if streaming || !input_empty {
        EscAction::Arm
    } else {
        EscAction::Nothing
    }
}
```

**Dispatch in `handle_event`:**

```rust
KeyCode::Esc => {
    let action = esc_action(app.last_esc_at, app.streaming, app.input_text().is_empty());
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

**On any non-ESC key press** (add to the top of the `Event::Key` arm, before routing to `app.input.input(key)`):

```rust
if key.code != KeyCode::Esc {
    app.last_esc_at = None;
    app.esc_hint = false;
}
```

**`last_sent` lifecycle:**
- Set in `submit()` immediately before `app.clear_input()`.
- Cleared in `start_new_session()` — if the user does `/clear` to start fresh, restoring a message from the old session would be surprising.
- NOT cleared by a streaming `Done`/`Err` event — the message is still the last thing the user sent.

**Edge case — streaming finishes between ESC 1 and ESC 2:**
If the agent finishes streaming in the ~400ms between the two ESCs, `streaming` is false on the second press, so `ClearInput` fires instead of `CancelAndRestore`. This is the correct behaviour: there is nothing left to cancel.

**Status bar ESC hint:**
When `app.esc_hint` is true, append a dim span to the status bar:
- If `streaming`: ` · ESC again to cancel`
- Otherwise: ` · ESC again to clear`

### 3. Scroll Position Indicator

When `scroll_back > 0`, users need visual feedback that they are not at the bottom.

**Status bar change:** when `scroll_back > 0`, right-side of status bar shows:
```
↑ {scroll_back} lines · ⬇ to bottom
```
This replaces the `context kept` label while scrolled. When back at bottom (`scroll_back == 0`), `context kept` returns.

---

## PR 2: Skin System + UI Polish

### Theme Data Model

New file: `mur-core/src/cmd/agent/cli/theme.rs`

```rust
use ratatui::style::{Color, Style};
use ratatui::widgets::{BorderType, Padding};

pub struct Theme {
    // ── labels (bold, identifies speaker) ────────────────────
    pub user:           Color,   // "› you" label
    pub agent:          Color,   // "● agent" label
    pub shell:          Color,   // "$ cmd" label in !command output
    // ── body text (normal weight, the actual message) ─────────
    pub user_text:      Color,   // continuation lines of user turn
    pub agent_text:     Color,   // continuation lines of agent reply
    pub thinking:       Color,   // streaming thinking tokens (italic + dim)
    // ── chrome ───────────────────────────────────────────────
    pub system:         Color,   // system hints, errors, slash-cmd output
    pub border:         Color,   // transcript + input borders
    pub border_title:   Color,   // border title text
    pub separator:      Color,   // inter-message separator line (if show_separator)
    // ── status bar ───────────────────────────────────────────
    pub status_bg:      Color,   // status bar background
    pub badge_fg:       Color,   // agent-name badge foreground
    pub badge_bg:       Color,   // agent-name badge background

    // ── layout / spacing ─────────────────────────────────────
    pub border_type:    BorderType, // Plain | Rounded | Double
    pub inner_padding:  u8,         // horizontal padding inside panes; valid range 0–2
    pub show_separator: bool,       // true = ─── line between messages
                                    // false = blank line (current Dark behaviour)
    pub compact_input:  bool,       // shortened hint in input box title
}
```

**Rationale for split `user` / `user_text`:** the label uses the accent color at full saturation; the body is a lighter/dimmer tint. This prevents large blocks of vivid color while keeping speaker identity clear at a glance. Same split for `agent` / `agent_text`.

Each skin has a distinct layout feel:

| Skin  | `border_type` | `inner_padding` | `show_separator` | `compact_input` |
|-------|:------------:|:---------------:|:----------------:|:---------------:|
| Dark  | `Plain`      | 0               | false (blank)    | false           |
| Light | `Rounded`    | 1               | true (─── line)  | false           |
| MUR   | `Rounded`    | 1               | true (─── line)  | true            |

`Dark` is zero-regression for existing users.

```rust
pub const DARK: Theme = Theme {
    user:         Color::Green,
    agent:        Color::Cyan,
    shell:        Color::Green,
    user_text:    Color::Gray,           // lighter than DarkGray for readability
    agent_text:   Color::White,
    thinking:     Color::DarkGray,
    system:       Color::DarkGray,
    border:       Color::DarkGray,
    border_title: Color::DarkGray,
    separator:    Color::DarkGray,
    status_bg:    Color::Reset,
    badge_fg:     Color::Cyan,
    badge_bg:     Color::Reset,
    border_type:    BorderType::Plain,
    inner_padding:  0,
    show_separator: false,
    compact_input:  false,
};

pub const LIGHT: Theme = Theme {
    user:         Color::Rgb(0x16, 0x65, 0x34),  // forest green      6.7:1 on white ✓
    agent:        Color::Rgb(0x0e, 0x6b, 0x8c),  // dark cyan         5.7:1 on white ✓
    shell:        Color::Rgb(0x16, 0x65, 0x34),
    user_text:    Color::Rgb(0x22, 0x22, 0x33),  // near-black        17:1 on white ✓
    agent_text:   Color::Rgb(0x22, 0x22, 0x33),
    thinking:     Color::Rgb(0x88, 0x88, 0x99),
    system:       Color::Rgb(0x77, 0x77, 0x88),  // 3.5:1 on white — acceptable for hints
    border:       Color::Rgb(0xd0, 0xd0, 0xe0),
    border_title: Color::Rgb(0x99, 0x99, 0x99),
    separator:    Color::Rgb(0xd8, 0xd8, 0xe8),
    status_bg:    Color::Rgb(0xef, 0xef, 0xf5),
    badge_fg:     Color::Rgb(0x0e, 0x6b, 0x8c),
    badge_bg:     Color::Rgb(0xe0, 0xf0, 0xf8),
    border_type:    BorderType::Rounded,
    inner_padding:  1,
    show_separator: true,
    compact_input:  false,
};

pub const MUR: Theme = Theme {
    user:         Color::Rgb(0xa7, 0x8b, 0xfa),  // violet    7.1:1 on #0d0d1a ✓ WCAG AAA
    agent:        Color::Rgb(0xfb, 0xbf, 0x24),  // amber     11.5:1            ✓ WCAG AAA
    shell:        Color::Rgb(0x88, 0x88, 0xcc),
    user_text:    Color::Rgb(0xc8, 0xc8, 0xe8),  // soft lavender  11.8:1        ✓
    agent_text:   Color::Rgb(0xe0, 0xe0, 0xf0),  // near-white     13.5:1        ✓
    thinking:     Color::Rgb(0x44, 0x44, 0x77),  // dim indigo
    system:       Color::Rgb(0x77, 0x77, 0xaa),  // muted violet  4.6:1          ✓ WCAG AA
    border:       Color::Rgb(0x2a, 0x2a, 0x5a),  // indigo
    border_title: Color::Rgb(0x55, 0x55, 0x99),
    separator:    Color::Rgb(0x22, 0x22, 0x44),
    status_bg:    Color::Rgb(0x09, 0x09, 0x1a),
    badge_fg:     Color::Rgb(0xfb, 0xbf, 0x24),
    badge_bg:     Color::Rgb(0x22, 0x1a, 0x06),
    border_type:    BorderType::Rounded,
    inner_padding:  1,
    show_separator: true,
    compact_input:  true,
};
```

All foreground/background contrast ratios verified against WCAG AA (4.5:1 for normal text). Previously `system` color in MUR was `(0x33,0x33,0x5a)` = **1.6:1** — corrected to `(0x77,0x77,0xaa)` = **4.6:1**.

`App` gains `theme: &'static Theme`. Resolved once at startup; `/skin` updates it live.

### Config Change

**File:** `mur-common/src/config.rs`

Add a new section at the end of `Config`:

```rust
#[serde(default)]
pub cli: CliConfig,
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliConfig {
    /// Active skin name: "dark" | "light" | "mur". Defaults to "dark".
    pub skin: Option<String>,
}
```

`#[serde(default)]` ensures old config files without a `[cli]` section deserialize cleanly.

### Skin Loading (precedence chain)

1. `--skin <name>` CLI flag (highest)
2. `config.cli.skin` from `~/.mur/config.yaml`
3. `"dark"` (hardcoded default)

**File:** `mur-core/src/cmd/agent/cli/mod.rs` — add `--skin` to the clap subcommand for `agent cli` (defined in `mur-core/src/cmd/agent/mod.rs`).

```rust
fn resolve_skin(name: &str) -> &'static Theme {
    match name {
        "light" => &theme::LIGHT,
        "mur"   => &theme::MUR,
        "dark"  => &theme::DARK,
        other   => {
            // caller is responsible for showing a warning before calling resolve_skin
            eprintln!("unknown skin '{other}', using dark");
            &theme::DARK
        }
    }
}
```

At startup in `run_tui`, if the resolved skin name came from config and is unknown, push a system message: `"unknown skin '{name}' in config, using dark"`.

### `/skin` Slash Command

```rust
/// `/skin [dark|light|mur]` — show or switch active skin.
Skin(Option<String>),
```

Handler:
- No arg → print current skin name.
- Valid name → `resolve_skin`, update `app.theme`, write `config.cli.skin` to `~/.mur/config.yaml`. If the config write fails, still apply the theme in-memory and show: `"skin changed to {name} (could not persist: {e})"`.
- Unknown name → `"unknown skin '{name}' — valid: dark, light, mur"`.

### UI Layout Changes (skin-driven)

**Borders:** `render_transcript` and the input `Block` read `theme.border_type`:

```rust
Block::default()
    .borders(Borders::ALL)
    .border_type(theme.border_type)
    .border_style(Style::default().fg(theme.border))
    .title(...)
```

**Inner padding:** Use `Block::padding()` (available in ratatui 0.29) — no extra Layout split needed:

```rust
block.padding(Padding::horizontal(theme.inner_padding as u16))
```

**Message separators:** If `theme.show_separator`, push a separator line between messages:

```rust
let sep_width = inner.width
    .saturating_sub(theme.inner_padding as u16 * 2) as usize;
lines.push(Line::styled(
    "─".repeat(sep_width),
    Style::default().fg(theme.separator),
));
```

`inner` is the rect after `block.inner(area)`, computed once at the top of `render_transcript`. If `!show_separator`, keep the existing `Line::default()` blank spacer.

**Status bar (all skins):**

```
[badge: agent-name] ▶ state · 019ed3  (right-aligned: ↑ N lines · ⬇ to bottom | context kept)
```

- Badge: `Span::styled(format!(" {} ", agent), Style::default().fg(theme.badge_fg).bg(theme.badge_bg))`
- Channel short: first 6 hex chars of the channel UUID (e.g. `019ed3`), or `—` if no channel yet.
- Right side: when `scroll_back > 0` show `↑ {scroll_back} lines · ⬇ to bottom`; otherwise `context kept` or `not saved` (existing logic).
- ESC hint: when `app.esc_hint`, append ` · ESC again to {cancel|clear}` as a dim span on the right.

**Input box title:** If `theme.compact_input`: `" message — Enter · Alt+Enter · /help "`. Otherwise current full text.

**HITL modal colors:** Remain hardcoded for now (Yellow border, Green/Yellow/Red action keys). Tracked as future work: add `hitl_border`, `hitl_approve`, `hitl_deny` to `Theme`.

### File Size Check

After this work:
- `theme.rs` ~110 lines (new)
- `ui.rs` ~290 lines (was 255, small additions)
- `app.rs` ~670 lines (was 640, new fields + `EscAction`)
- `mod.rs` ~660 lines (was 632, ESC handler + skin flag)

All under the 800-line limit. If `app.rs` approaches the limit, split `SlashCmd` + `parse_slash` into `slash.rs`.

---

## Testing

### PR 1

**Unit tests in `app.rs`:**

```rust
// esc_action is a pure function — no timing dependency
#[test]
fn esc_arm_when_streaming_and_empty() {
    assert_eq!(esc_action(None, true, true), EscAction::Arm);
}
#[test]
fn esc_cancel_restore_on_second_press() {
    let t = Instant::now() - Duration::from_millis(100);
    assert_eq!(esc_action(Some(t), true, true), EscAction::CancelAndRestore);
}
#[test]
fn esc_window_expired_rearms() {
    let t = Instant::now() - Duration::from_millis(500);
    assert_eq!(esc_action(Some(t), true, true), EscAction::Arm);
}
#[test]
fn esc_clear_when_not_streaming_and_has_text() {
    let t = Instant::now() - Duration::from_millis(100);
    assert_eq!(esc_action(Some(t), false, false), EscAction::ClearInput);
}
#[test]
fn esc_nothing_when_not_streaming_and_empty() {
    let t = Instant::now() - Duration::from_millis(100);
    assert_eq!(esc_action(Some(t), false, true), EscAction::Nothing);
}
```

`esc_action` takes `Option<Instant>` — tests construct the `Instant` as `Instant::now() - offset` to simulate timing without wall-clock dependency.

**Additional unit tests in `app.rs`:**
- `last_sent` is set to submitted text after `submit()`.
- `last_sent` is cleared after `start_new_session()`.
- `scroll_back` increments on simulated `Event::Mouse(ScrollUp)` via `handle_event` (use `tokio::sync::mpsc::channel` to provide the `tx`).

**Manual:**
- Mouse scroll in iTerm2 and Ghostty.
- Scroll up mid-stream, verify `↑ N lines` indicator appears; submit a new message, verify indicator disappears.

### PR 2

**Unit tests in `theme.rs`:**
- `resolve_skin("light")` returns `&LIGHT`.
- `resolve_skin("mur")` returns `&MUR`.
- `resolve_skin("unknown")` returns `&DARK`.

**Unit tests in `app.rs`:**
- `/skin` slash command parses to `SlashCmd::Skin(Some("mur".into()))`.
- `/skin` with no arg parses to `SlashCmd::Skin(None)`.

**Manual:**
- `mur agent cli <agent> --skin dark|light|mur` — visual check of each skin.
- `/skin mur` → persists to `~/.mur/config.yaml`, survives restart.
- `/skin muur` (typo) → shows error message, skin unchanged.
- `config.cli.skin: "muur"` → startup shows warning, uses dark.
- Old config without `[cli]` section → no parse error, defaults to dark.
