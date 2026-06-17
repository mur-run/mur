# Agent CLI UX — PR 2: Skin System + UI Polish

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a three-skin theming system (dark/light/mur) to the agent CLI, with per-skin colour, border style, padding, and separator settings. Skin is selected via `--skin` flag or `/skin` slash command and persisted to `~/.mur/config.yaml`.

**Architecture:** `Theme` is a pure data struct (`&'static` const) threaded through all render functions. `App` holds `theme: &'static Theme`. `/skin` updates `app.theme` at runtime and writes config. `ui.rs` reads theme fields instead of hardcoded color constants. PR 1 must be merged first.

**Tech Stack:** Rust, ratatui 0.29 (`BorderType`, `Padding`), serde_yaml, clap

**Spec:** `docs/superpowers/specs/2026-06-17-agent-cli-ux-improvements-design.md` — PR 2 section

---

## File Map

| File | Change |
|------|--------|
| `mur-common/src/config.rs` | Add `CliConfig` struct + `cli` field to `Config` |
| `mur-core/src/cmd/agent/cli/theme.rs` | **NEW** — `Theme` struct, 3 const instances, `resolve_skin()`, `skin_name()` |
| `mur-core/src/cli/agent.rs` | Add `--skin` to `AgentAction::Cli` |
| `mur-core/src/dispatch.rs` | Thread `skin` from `AgentAction::Cli` to `cmd_cli()` |
| `mur-core/src/cmd/agent/cli/mod.rs` | Accept `skin`, load from config/flag, pass to `App`; add `/skin` handler; add `persist_skin()` |
| `mur-core/src/cmd/agent/cli/app.rs` | Add `theme` field to `App`, `App::new()` takes `theme`; add `Skin(Option<String>)` to `SlashCmd` |
| `mur-core/src/cmd/agent/cli/ui.rs` | Thread `theme` through all render fns; remove hardcoded color consts; use `border_type`, `Padding`, separators |

---

### Task 1: Add CliConfig to mur-common

**Files:**
- Modify: `mur-common/src/config.rs`

- [ ] **Step 1: Add CliConfig struct**

Open `mur-common/src/config.rs`. Find the `Config` struct (around line 14). Add this new struct somewhere before `Config` (e.g., just before it):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CliConfig {
    /// Active skin for `mur agent cli`. Values: "dark" | "light" | "mur".
    /// `None` means use the built-in default ("dark").
    pub skin: Option<String>,
}
```

- [ ] **Step 2: Add `cli` field to Config struct**

Inside `pub struct Config { ... }`, add at the end (before the closing `}`):

```rust
    #[serde(default)]
    pub cli: CliConfig,
```

`#[serde(default)]` ensures old `~/.mur/config.yaml` files without a `[cli]` section deserialise cleanly without errors.

- [ ] **Step 3: Build mur-common**

```bash
cargo build -p mur-common 2>&1
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(config): add CliConfig with skin field"
```

---

### Task 2: Create theme.rs

**Files:**
- Create: `mur-core/src/cmd/agent/cli/theme.rs`

- [ ] **Step 1: Create the file**

```rust
//! Skin/theme definitions for the agent CLI TUI.

use ratatui::style::Color;
use ratatui::widgets::BorderType;

pub struct Theme {
    // ── labels (bold, identifies speaker) ────────────────────────────────────
    pub user:         Color,  // "› you" label
    pub agent:        Color,  // "● agent" label
    pub shell:        Color,  // "$ cmd" label for !command output
    // ── body text ─────────────────────────────────────────────────────────────
    pub user_text:    Color,  // continuation lines of a user turn
    pub agent_text:   Color,  // continuation lines of an agent reply
    pub thinking:     Color,  // streaming thinking tokens (italic+dim)
    // ── chrome ────────────────────────────────────────────────────────────────
    pub system:       Color,  // system hints, errors, slash-cmd output
    pub border:       Color,  // transcript + input box borders
    pub border_title: Color,  // text inside the border title
    pub separator:    Color,  // inter-message separator line
    // ── status bar ────────────────────────────────────────────────────────────
    pub status_bg:    Color,  // status bar background
    pub badge_fg:     Color,  // agent-name badge foreground
    pub badge_bg:     Color,  // agent-name badge background
    // ── layout ────────────────────────────────────────────────────────────────
    pub border_type:    BorderType,  // Plain | Rounded | Double
    pub inner_padding:  u8,          // horizontal padding inside panes (0–2)
    pub show_separator: bool,        // true = ─── line; false = blank line
    pub compact_input:  bool,        // shorten input box hint text
}

pub const DARK: Theme = Theme {
    user:         Color::Green,
    agent:        Color::Cyan,
    shell:        Color::Green,
    user_text:    Color::Gray,
    agent_text:   Color::White,
    thinking:     Color::DarkGray,
    system:       Color::DarkGray,
    border:       Color::DarkGray,
    border_title: Color::DarkGray,
    separator:    Color::DarkGray,
    status_bg:    Color::Reset,
    badge_fg:     Color::Black,
    badge_bg:     Color::Cyan,
    border_type:    BorderType::Plain,
    inner_padding:  0,
    show_separator: false,
    compact_input:  false,
};

pub const LIGHT: Theme = Theme {
    user:         Color::Rgb(0x16, 0x65, 0x34),  // forest green  6.7:1 on white ✓
    agent:        Color::Rgb(0x0e, 0x6b, 0x8c),  // dark cyan     5.7:1 on white ✓
    shell:        Color::Rgb(0x16, 0x65, 0x34),
    user_text:    Color::Rgb(0x22, 0x22, 0x33),  // near-black   17:1 ✓
    agent_text:   Color::Rgb(0x22, 0x22, 0x33),
    thinking:     Color::Rgb(0x88, 0x88, 0x99),
    system:       Color::Rgb(0x77, 0x77, 0x88),  // 3.5:1 — acceptable for hints
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
    user:         Color::Rgb(0xa7, 0x8b, 0xfa),  // violet   7.1:1 ✓ WCAG AAA
    agent:        Color::Rgb(0xfb, 0xbf, 0x24),  // amber   11.5:1 ✓ WCAG AAA
    shell:        Color::Rgb(0x88, 0x88, 0xcc),
    user_text:    Color::Rgb(0xc8, 0xc8, 0xe8),  // soft lavender 11.8:1 ✓
    agent_text:   Color::Rgb(0xe0, 0xe0, 0xf0),  // near-white    13.5:1 ✓
    thinking:     Color::Rgb(0x44, 0x44, 0x77),
    system:       Color::Rgb(0x77, 0x77, 0xaa),  // muted violet  4.6:1 ✓ WCAG AA
    border:       Color::Rgb(0x2a, 0x2a, 0x5a),
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

const KNOWN: [(&str, &Theme); 3] = [("dark", &DARK), ("light", &LIGHT), ("mur", &MUR)];

/// Resolve a skin name to a theme. Falls back to `&DARK` for unknown names.
/// Callers should warn the user when the name is not recognised.
pub fn resolve_skin(name: &str) -> &'static Theme {
    KNOWN
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| *t)
        .unwrap_or(&DARK)
}

/// Return the canonical name of a theme instance, or "dark" as fallback.
pub fn skin_name(theme: &'static Theme) -> &'static str {
    KNOWN
        .iter()
        .find(|(_, t)| std::ptr::eq(*t, theme))
        .map(|(n, _)| *n)
        .unwrap_or("dark")
}

/// True if `name` is a valid skin name.
pub fn is_known_skin(name: &str) -> bool {
    KNOWN.iter().any(|(n, _)| *n == name)
}
```

- [ ] **Step 2: Declare the module in mod.rs**

Open `mur-core/src/cmd/agent/cli/mod.rs`. Find the module declarations at the top (lines 9–15):

```rust
mod app;
mod manage;
mod markdown;
mod multiplex;
pub mod persist;
mod stream;
mod ui;
```

Add `mod theme;` to this list (alphabetical order, before `mod ui`):

```rust
mod app;
mod manage;
mod markdown;
mod multiplex;
pub mod persist;
mod stream;
mod theme;
mod ui;
```

- [ ] **Step 3: Write unit tests for theme helpers**

Add a `#[cfg(test)]` block at the bottom of `theme.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_skins() {
        assert!(std::ptr::eq(resolve_skin("dark"),  &DARK));
        assert!(std::ptr::eq(resolve_skin("light"), &LIGHT));
        assert!(std::ptr::eq(resolve_skin("mur"),   &MUR));
    }

    #[test]
    fn resolve_unknown_falls_back_to_dark() {
        assert!(std::ptr::eq(resolve_skin("neon"), &DARK));
        assert!(std::ptr::eq(resolve_skin(""),     &DARK));
    }

    #[test]
    fn skin_name_round_trips() {
        assert_eq!(skin_name(&DARK),  "dark");
        assert_eq!(skin_name(&LIGHT), "light");
        assert_eq!(skin_name(&MUR),   "mur");
    }

    #[test]
    fn is_known_skin_validates_names() {
        assert!(is_known_skin("dark"));
        assert!(is_known_skin("light"));
        assert!(is_known_skin("mur"));
        assert!(!is_known_skin("neon"));
        assert!(!is_known_skin("DARK"));
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo nextest run -p mur-core theme 2>&1
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/theme.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): add Theme struct and DARK/LIGHT/MUR skin constants"
```

---

### Task 3: Add --skin CLI flag and thread it through to run_tui

**Files:**
- Modify: `mur-core/src/cli/agent.rs`
- Modify: `mur-core/src/dispatch.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

- [ ] **Step 1: Add --skin to AgentAction::Cli**

Open `mur-core/src/cli/agent.rs`. Find `AgentAction::Cli { ... }` (line 75–85):

```rust
    Cli {
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        auto: bool,
    },
```

Change to:

```rust
    Cli {
        /// Agent name(s) — more than one opens each chat in its own split pane
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Resume the most recent saved conversation for this agent
        #[arg(long)]
        resume: bool,
        /// Auto-approve every tool call for this session (no HITL prompts)
        #[arg(long)]
        auto: bool,
        /// Visual skin: dark (default) | light | mur
        #[arg(long)]
        skin: Option<String>,
    },
```

- [ ] **Step 2: Thread skin through dispatch.rs**

Open `mur-core/src/dispatch.rs` around line 1108. Change:

```rust
        AgentAction::Cli {
            names,
            resume,
            auto,
        } => cmd::agent::cmd_cli(&names, resume, auto).await?,
```

To:

```rust
        AgentAction::Cli {
            names,
            resume,
            auto,
            skin,
        } => cmd::agent::cmd_cli(&names, resume, auto, skin).await?,
```

- [ ] **Step 3: Update cmd_cli signature and pass skin to run_tui**

Open `mur-core/src/cmd/agent/cli/mod.rs`. Change line 51:

```rust
pub async fn cmd_cli(names: &[String], resume: bool, auto: bool) -> Result<()> {
```

To:

```rust
pub async fn cmd_cli(names: &[String], resume: bool, auto: bool, skin: Option<String>) -> Result<()> {
```

Inside `cmd_cli`, the final line is:

```rust
    run_tui(home, agent, resume, auto).await
```

Change to:

```rust
    run_tui(home, agent, resume, auto, skin).await
```

Also update the multiplex path at line 54:

```rust
        return tokio::task::spawn_blocking(move || multiplex::run(&names, resume, auto)).await?;
```

Change to (multiplex ignores skin for now — uses dark):

```rust
        return tokio::task::spawn_blocking(move || multiplex::run(&names, resume, auto)).await?;
```

(No change needed for multiplex in this PR.)

- [ ] **Step 4: Update run_tui signature and add skin loading**

Find `async fn run_tui(home: PathBuf, agent: String, resume: bool, auto: bool) -> Result<()>` (around line 105). Change signature to:

```rust
async fn run_tui(
    home: PathBuf,
    agent: String,
    resume: bool,
    auto: bool,
    skin: Option<String>,
) -> Result<()> {
```

At the TOP of `run_tui`, before any existing code, add:

```rust
    // Resolve skin: CLI flag > config > "dark"
    let cfg = mur_common::Config::load_or_default(&home.join("config.yaml"));
    let skin_name = skin.as_deref()
        .or_else(|| cfg.cli.skin.as_deref())
        .unwrap_or("dark");
    let unknown_skin = !theme::is_known_skin(skin_name);
    let active_theme = theme::resolve_skin(skin_name);
```

- [ ] **Step 5: Build**

```bash
cargo build -p mur-core 2>&1
```

Expected: no errors. (There will be errors about `App::new` receiving wrong argument count — those are fixed in Task 4.)

- [ ] **Step 6: Commit (partial — will fix App::new in Task 4)**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/dispatch.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): add --skin flag and thread through to run_tui"
```

---

### Task 4: Add theme field to App and update App::new callers

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

- [ ] **Step 1: Add use for Theme in app.rs**

At the top of `app.rs`, add:

```rust
use super::theme::Theme;
```

- [ ] **Step 2: Add theme field to App struct**

Inside `pub struct App { ... }`, add after `cwd_sent: bool` (but before the ESC fields added in PR 1):

```rust
    /// Active visual skin, resolved at startup. Updated live by `/skin`.
    pub theme: &'static Theme,
```

- [ ] **Step 3: Update App::new() signature**

Change:

```rust
    pub fn new(home: PathBuf, agent: String, session: Session) -> Self {
```

To:

```rust
    pub fn new(home: PathBuf, agent: String, session: Session, theme: &'static Theme) -> Self {
```

Inside `Self { ... }`, add after `cwd_sent: false`:

```rust
            theme,
```

- [ ] **Step 4: Update App::new() callers in mod.rs**

In `run_tui`, find the calls to `App::new(...)`. There are two — one for resume and one for fresh start. Both look like:

```rust
App::new(home.to_path_buf(), agent.to_string(), Session::create(home, agent)?)
```

Change each to pass `active_theme`:

```rust
App::new(home.to_path_buf(), agent.to_string(), Session::create(home, agent)?, active_theme)
```

Do the same for the resume path:

```rust
App::new(home.to_path_buf(), agent.to_string(), session, active_theme)
```

(Search for all `App::new(` calls in `mod.rs` and add `active_theme` as the last argument to each.)

- [ ] **Step 5: Add the unknown skin warning after App creation**

In `run_tui`, after `let mut app = ...` (or after `init_app(...)` depending on the resume path), add:

```rust
    if unknown_skin {
        app.push_system(format!(
            "unknown skin '{skin_name}', using dark — valid: dark, light, mur"
        ));
    }
```

- [ ] **Step 6: Update tests in app.rs**

The `app()` and `app_at()` test helpers call `App::new(...)`. Add `&super::theme::DARK` as the last argument:

Find:

```rust
    fn app() -> App {
        let home = tempdir().unwrap();
        let session = Session::create(home.path(), "a").unwrap();
        App::new(home.path().to_path_buf(), "a".into(), session)
    }

    fn app_at(home: &tempfile::TempDir) -> App {
        let session = Session::create(home.path(), "a").unwrap();
        App::new(home.path().to_path_buf(), "a".into(), session)
    }
```

Change both to:

```rust
    fn app() -> App {
        let home = tempdir().unwrap();
        let session = Session::create(home.path(), "a").unwrap();
        App::new(home.path().to_path_buf(), "a".into(), session, &super::theme::DARK)
    }

    fn app_at(home: &tempfile::TempDir) -> App {
        let session = Session::create(home.path(), "a").unwrap();
        App::new(home.path().to_path_buf(), "a".into(), session, &super::theme::DARK)
    }
```

- [ ] **Step 7: Build and run tests**

```bash
cargo build -p mur-core 2>&1
cargo nextest run -p mur-core 2>&1
```

Expected: builds clean, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): add theme field to App, thread active_theme through"
```

---

### Task 5: Add Skin slash command + persist_skin

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

- [ ] **Step 1: Add Skin variant to SlashCmd enum**

In `app.rs`, add to the `SlashCmd` enum:

```rust
    /// `/skin [dark|light|mur]` — show or switch the active skin (persists to config).
    Skin(Option<String>),
```

- [ ] **Step 2: Add skin to parse_slash()**

In `parse_slash()`, add after the `"auto"` arm:

```rust
        "skin" | "theme" => SlashCmd::Skin(words.next().map(str::to_string)),
```

- [ ] **Step 3: Add "/skin" to SLASH_COMMANDS**

The current array has 10 entries. Change to 11:

```rust
pub const SLASH_COMMANDS: [&str; 11] = [
    "/help",
    "/clear",
    "/card",
    "/sessions",
    "/channels",
    "/auto",
    "/mcp",
    "/skill",
    "/skin",
    "/exit",
    "/quit",
];
```

- [ ] **Step 4: Write a unit test for parse_slash skin**

In the `#[cfg(test)]` block:

```rust
    #[test]
    fn parse_slash_skin_variants() {
        assert_eq!(parse_slash("/skin"), Some(SlashCmd::Skin(None)));
        assert_eq!(parse_slash("/skin mur"), Some(SlashCmd::Skin(Some("mur".into()))));
        assert_eq!(parse_slash("/theme light"), Some(SlashCmd::Skin(Some("light".into()))));
    }
```

- [ ] **Step 5: Run the test**

```bash
cargo nextest run -p mur-core parse_slash_skin 2>&1
```

Expected: 1 test passes.

- [ ] **Step 6: Add persist_skin() function in mod.rs**

In `mod.rs`, add this function near the other helper functions (e.g., after `complete_slash`):

```rust
/// Write `cli.skin = name` to `~/.mur/config.yaml` atomically.
fn persist_skin(home: &std::path::Path, name: &str) -> anyhow::Result<()> {
    use mur_common::Config;
    let path = home.join("config.yaml");
    let mut cfg = Config::load_or_default(&path);
    cfg.cli.skin = Some(name.to_string());
    let text = serde_yaml::to_string(&cfg).context("serialise config")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, &text).context("write config tmp")?;
    std::fs::rename(&tmp, &path).context("rename config")?;
    Ok(())
}
```

- [ ] **Step 7: Handle SlashCmd::Skin in handle_slash()**

In `handle_slash()`, add the `Skin` arm:

```rust
        SlashCmd::Skin(name_opt) => match name_opt {
            None => {
                let current = theme::skin_name(app.theme);
                app.push_system(format!("current skin: {current} — valid: dark, light, mur"));
            }
            Some(name) => {
                if !theme::is_known_skin(&name) {
                    app.push_system(format!(
                        "unknown skin '{name}' — valid: dark, light, mur"
                    ));
                } else {
                    app.theme = theme::resolve_skin(&name);
                    let h = app.home.clone();
                    match persist_skin(&h, &name) {
                        Ok(()) => app.push_system(format!("skin changed to {name}")),
                        Err(e) => app.push_system(format!(
                            "skin changed to {name} (could not persist: {e})"
                        )),
                    }
                }
            }
        },
```

- [ ] **Step 8: Build**

```bash
cargo build -p mur-core 2>&1
```

Expected: no errors.

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(agent-cli): /skin slash command + persist to config"
```

---

### Task 6: Update ui.rs to use Theme

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/ui.rs`

This is the main visual refactor. The current file uses three hardcoded constants `USER`, `AGENT`, `SYSTEM`. We replace those with per-call lookups from `app.theme`.

- [ ] **Step 1: Add imports for BorderType and Padding**

At the top of `ui.rs`, the current ratatui imports are:

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
```

Change to:

```rust
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};
```

- [ ] **Step 2: Remove the three hardcoded color constants**

Delete lines 12–14:

```rust
const USER: Color = Color::Green;
const AGENT: Color = Color::Cyan;
const SYSTEM: Color = Color::DarkGray;
```

(The compiler will now error on every use of `USER`, `AGENT`, `SYSTEM` — each error points to where a theme reference needs to go.)

- [ ] **Step 3: Update render_transcript() to use theme**

The current `render_transcript` signature is `fn render_transcript(f: &mut Frame, app: &App, area: Rect)`. It accesses `app.theme`. No signature change needed.

Replace the function body:

```rust
fn render_transcript(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(theme.border_type)
        .border_style(Style::default().fg(theme.border))
        .padding(Padding::horizontal(theme.inner_padding as u16))
        .title(format!(" chat · {} ", app.agent))
        .title_style(Style::default().fg(theme.border_title));
    let inner = block.inner(area);

    let mut lines: Vec<Line> = Vec::new();
    let msg_count = app.messages.len();
    for (i, m) in app.messages.iter().enumerate() {
        push_message(&mut lines, m, app.spinner, theme);
        // Separator between messages (not after the last one)
        if i + 1 < msg_count {
            if theme.show_separator {
                let sep_width = inner.width as usize;
                lines.push(Line::styled(
                    "─".repeat(sep_width),
                    Style::default().fg(theme.separator),
                ));
            } else {
                lines.push(Line::default());
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::styled(
            "Say hello — type below and press Enter.",
            Style::default().fg(theme.system),
        ));
    }

    let total = lines.len() as u16;
    let visible = inner.height;
    let max_off = total.saturating_sub(visible);
    let offset = max_off.saturating_sub(app.scroll_back);

    let output = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    f.render_widget(output, area);
}
```

- [ ] **Step 4: Update push_message() to accept and use theme**

Change the signature from `fn push_message(lines: &mut Vec<Line<'static>>, m: &ChatMsg, spinner: usize)` to:

```rust
fn push_message(
    lines: &mut Vec<Line<'static>>,
    m: &ChatMsg,
    spinner: usize,
    theme: &'static super::theme::Theme,
) {
```

Then update all color references inside the function. The complete new body:

```rust
    match m.role {
        Role::User => {
            let mut it = m.text.lines();
            if let Some(first) = it.next() {
                lines.push(Line::styled(
                    first.to_string(),
                    Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(l.to_string(), Style::default().fg(theme.user_text)));
            }
        }
        Role::Shell => {
            let mut it = m.text.lines();
            if let Some(first) = it.next() {
                lines.push(Line::styled(
                    first.to_string(),
                    Style::default().fg(theme.shell).add_modifier(Modifier::BOLD),
                ));
            }
            for l in it {
                lines.push(Line::styled(l.to_string(), Style::default().fg(theme.user_text)));
            }
        }
        Role::System => {
            for l in m.text.lines() {
                lines.push(Line::styled(
                    l.to_string(),
                    Style::default().fg(theme.system).add_modifier(Modifier::ITALIC),
                ));
            }
        }
        Role::Agent => {
            lines.push(Line::from(Span::styled(
                "● agent",
                Style::default().fg(theme.agent).add_modifier(Modifier::BOLD),
            )));
            if m.streaming {
                if !m.thinking.is_empty() {
                    for l in m.thinking.lines() {
                        lines.push(Line::styled(
                            l.to_string(),
                            Style::default()
                                .fg(theme.thinking)
                                .add_modifier(Modifier::ITALIC | Modifier::DIM),
                        ));
                    }
                }
                let mut body: Vec<Line> = m.text.lines().map(|l| Line::raw(l.to_string())).collect();
                let spin = SPINNER[spinner % SPINNER.len()];
                match body.last_mut() {
                    Some(last) => last
                        .spans
                        .push(Span::styled(format!(" {spin}"), Style::default().fg(theme.agent))),
                    None => body.push(Line::styled(spin.to_string(), Style::default().fg(theme.agent))),
                }
                lines.extend(body);
            } else if let Some(cached) = &m.rendered {
                lines.extend(cached.iter().cloned());
            } else {
                lines.extend(super::markdown::render(&m.text).lines);
            }
        }
    }
```

Note: the trailing `lines.push(Line::default())` that used to be at the end of each branch is REMOVED — separator logic is now in `render_transcript`.

- [ ] **Step 5: Update render_status() to use theme**

The `render_status` function was updated in PR 1 to use the `AGENT` and `SYSTEM` constants — replace those with `theme` lookups.

Change `fn render_status(f: &mut Frame, app: &App, area: Rect)` body:

Replace `AGENT` with `app.theme.agent`, `SYSTEM` with `app.theme.system`, and update the badge to use `app.theme.badge_fg`/`badge_bg`:

```rust
fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let (msg, color) = if app.hitl.is_some() {
        (
            "tool approval needed — [y] approve · [a] always (session) · [n] deny".to_string(),
            Color::Yellow,
        )
    } else if app.streaming {
        let spin = SPINNER[app.spinner % SPINNER.len()];
        (format!("{spin} generating… Ctrl+C to cancel"), theme.agent)
    } else {
        let ctx = if app.context_task_id.is_some() { " · context kept" } else { "" };
        (format!("ready{ctx}"), theme.system)
    };

    let right_hint: Option<(String, Color)> = if app.scroll_back > 0 {
        Some((format!("↑ {} lines · ⬇ to bottom", app.scroll_back), theme.system))
    } else if app.esc_hint {
        let hint = if app.streaming { "ESC again to cancel" } else { "ESC again to clear" };
        Some((hint.to_string(), theme.system))
    } else {
        None
    };

    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.agent),
            Style::default().fg(theme.badge_fg).bg(theme.badge_bg),
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
            Style::default().fg(theme.agent),
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

- [ ] **Step 6: Update sync_input_block() in app.rs to use theme**

In `app.rs`, `sync_input_block()` currently builds blocks with hardcoded `Color::Red` for shell mode. The method now accesses `self.theme`:

```rust
    pub fn sync_input_block(&mut self) {
        let theme = self.theme;
        let hint = if theme.compact_input {
            " message — Enter · Alt+Enter · /help "
        } else {
            " message — Enter to send · Alt+Enter newline · /help · Ctrl+D quit "
        };
        let is_shell = self.input_text().trim_start().starts_with('!');
        let block = if is_shell {
            Block::default()
                .borders(Borders::ALL)
                .border_type(theme.border_type)
                .border_style(Style::default().fg(Color::Red))
                .padding(Padding::horizontal(theme.inner_padding as u16))
                .title(" ! shell command — output shared with agent ")
        } else {
            Block::default()
                .borders(Borders::ALL)
                .border_type(theme.border_type)
                .border_style(Style::default().fg(theme.border))
                .padding(Padding::horizontal(theme.inner_padding as u16))
                .title(hint)
                .title_style(Style::default().fg(theme.border_title))
        };
        self.input.set_block(block);
    }
```

Also update `new_input()` (which sets the initial block before the theme is available) to use plain/dark defaults:

```rust
fn new_input() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(" message — Enter to send · Alt+Enter newline · /help · Ctrl+D quit "),
    );
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text("Type a message…");
    ta.set_placeholder_style(Style::default().fg(Color::DarkGray));
    ta
}
```

(The block is immediately replaced by `sync_input_block()` on the first render loop tick, so the initial block is only visible for one frame and doesn't need theming.)

- [ ] **Step 7: Add Padding import to app.rs**

At top of `app.rs`:

```rust
use ratatui::widgets::{Block, Borders, Padding};
```

- [ ] **Step 8: Build — expect clean**

```bash
cargo build -p mur-core 2>&1
```

Expected: no errors.

- [ ] **Step 9: Run full test suite**

```bash
cargo nextest run -p mur-core 2>&1
```

Expected: all tests pass.

- [ ] **Step 10: Commit**

```bash
git add mur-core/src/cmd/agent/cli/ui.rs mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(agent-cli): thread Theme through ui.rs — border_type, padding, separator, colors"
```

---

### Task 7: Manual verification of all three skins

- [ ] **Step 1: Start an agent and test dark skin (default)**

```bash
cargo build -p mur-core && mur agent cli <name>
```

Expected: UI looks identical to before this PR (zero regression).

- [ ] **Step 2: Test light skin via flag**

```bash
mur agent cli <name> --skin light
```

Expected: white/ivory background, rounded borders, forest-green user label, navy agent label, separator lines between messages.

- [ ] **Step 3: Test MUR skin via flag**

```bash
mur agent cli <name> --skin mur
```

Expected: deep navy/indigo background, violet user labels, amber agent label, rounded borders, separator lines.

- [ ] **Step 4: Test /skin command at runtime**

Inside the CLI, type `/skin mur`. Expected: skin switches live, system message "skin changed to mur", config written.

- [ ] **Step 5: Verify config persistence**

```bash
grep -A2 'cli:' ~/.mur/config.yaml
```

Expected: shows `skin: mur`.

- [ ] **Step 6: Verify persisted skin loads on restart**

Exit the CLI and re-open without `--skin`. Expected: MUR skin loads.

- [ ] **Step 7: Test unknown skin warning**

```bash
mur agent cli <name> --skin neon
```

Expected: TUI opens with dark skin and shows system message: `unknown skin 'neon', using dark — valid: dark, light, mur`.

- [ ] **Step 8: Run lint**

```bash
cargo clippy -p mur-core -- -D warnings 2>&1
```

Fix any warnings before the final commit.

- [ ] **Step 9: Final commit if fixups needed**

```bash
git add -p
git commit -m "fix(agent-cli): <describe fixup>"
```
