# `mur agent cli` autocomplete (Type 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the one-shot `complete_slash()` in the `mur agent cli` TUI with a Claude-Code-style filtered completion menu over built-in slash commands (with a second subcommand layer) and the agent's skills, accepted with `Tab`/`Enter`.

**Architecture:** A new pure module `cli/complete.rs` holds the candidate model and a `compute(input, skills) -> Option<CompletionState>` function that derives the menu entirely from the current input text (so the menu is recomputed on every keystroke). `App` gains a `completion: Option<CompletionState>` (live menu) and a `skills: Vec<Candidate>` (loaded once at startup). `mod.rs` intercepts nav/accept/dismiss keys while the menu is open and refreshes it after every edit. `ui.rs` draws a floating `List` overlay above the input box.

**Tech Stack:** Rust 2024, ratatui 0.29, crossterm, tui_textarea. Single crate: `mur-core`. No runtime / A2A / `mur-common` changes.

## Global Constraints

- Brand name user-facing is uppercase **MUR**; CLI/command/slug stays lowercase `mur`. (No user-facing brand strings are added by this plan.)
- No hardcoded magic values — use named `const`s (`MAX_MENU_ROWS`, etc.).
- Single source file ≤ 800 lines; `complete.rs` is new and small, `mod.rs` only gains small functions.
- mur-core tests need `ORT_STRATEGY=download` in the environment (onnxruntime link). Run module-scoped: `ORT_STRATEGY=download cargo test -p mur-core complete`.
- Spec: `docs/superpowers/specs/2026-06-28-mur-agent-cli-autocomplete-design.md`.

---

## File Structure

- **Create** `mur-core/src/cmd/agent/cli/complete.rs` — candidate model, static command table, `compute()`/`filter()`/`skill_display_name()` (pure, unit-tested) + `load_agent_skills()` (startup I/O).
- **Modify** `mur-core/src/cmd/agent/cli/app.rs` — add `completion` + `skills` fields, init them, import the new types.
- **Modify** `mur-core/src/cmd/agent/cli/mod.rs` — declare `mod complete;`, load skills after `build_app`, intercept menu keys, refresh after edits, delete `complete_slash()`.
- **Modify** `mur-core/src/cmd/agent/cli/ui.rs` — render the popup overlay.

---

## Task 1: Pure completion module (`complete.rs`)

**Files:**
- Create: `mur-core/src/cmd/agent/cli/complete.rs`
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces (used by Tasks 2–4):
  - `pub struct Candidate { pub display: String, pub insert: String, pub desc: String, pub has_children: bool }` (derives `Debug, Clone, PartialEq`)
  - `pub struct CompletionState { pub items: Vec<Candidate>, pub selected: usize }`
  - `pub fn compute(input: &str, skills: &[Candidate]) -> Option<CompletionState>`
  - `pub fn load_agent_skills(agent: &str) -> Vec<Candidate>`

- [ ] **Step 1: Declare the module so it compiles**

In `mur-core/src/cmd/agent/cli/mod.rs`, add the module declaration alphabetically among the existing `mod` lines (after `mod bash_class;`, before `mod diff;` — line ~11):

```rust
mod complete;
```

- [ ] **Step 2: Write the failing tests**

Create `mur-core/src/cmd/agent/cli/complete.rs` with ONLY the test module first (it won't compile yet — that's the failing state):

```rust
//! Pure autocomplete logic for the `mur agent cli` completion menu: build the
//! candidate set for the current input and filter it. No TUI, no I/O here
//! (except `load_agent_skills`, which reads the agent profile at startup).

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str) -> Candidate {
        Candidate {
            display: name.into(),
            insert: name.into(),
            desc: String::new(),
            has_children: false,
        }
    }

    fn displays(state: &CompletionState) -> Vec<String> {
        state.items.iter().map(|c| c.display.clone()).collect()
    }

    #[test]
    fn no_menu_without_leading_slash() {
        assert!(compute("hello", &[skill("create-pr")]).is_none());
    }

    #[test]
    fn top_level_filters_commands_by_prefix_substring() {
        let s = compute("/sk", &[skill("create-pr")]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"/skill".to_string()));
        assert!(d.contains(&"/skin".to_string()));
        // "sk" does not match the skill "create-pr".
        assert!(!d.contains(&"create-pr".to_string()));
    }

    #[test]
    fn top_level_includes_matching_skills() {
        let s = compute("/cre", &[skill("create-pr")]).unwrap();
        assert_eq!(displays(&s), vec!["create-pr".to_string()]);
    }

    #[test]
    fn empty_slash_shows_commands_and_skills() {
        let s = compute("/", &[skill("create-pr")]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"/mcp".to_string()));
        assert!(d.contains(&"create-pr".to_string()));
    }

    #[test]
    fn descends_to_subcommands_after_space() {
        let s = compute("/mcp ", &[]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"list".to_string()));
        assert!(d.contains(&"add-remote".to_string()));
        let add = s.items.iter().find(|c| c.display == "list").unwrap();
        assert_eq!(add.insert, "/mcp list ");
        assert!(!add.has_children);
    }

    #[test]
    fn subcommands_filter_by_query() {
        let s = compute("/mcp add", &[]).unwrap();
        let d = displays(&s);
        assert!(d.contains(&"add".to_string()));
        assert!(d.contains(&"add-remote".to_string()));
        assert!(!d.contains(&"list".to_string()));
    }

    #[test]
    fn no_menu_past_layer_two() {
        assert!(compute("/mcp add foo", &[]).is_none());
    }

    #[test]
    fn command_without_subcommands_has_no_layer_two() {
        assert!(compute("/help ", &[]).is_none());
    }

    #[test]
    fn unknown_command_no_match_closes_menu() {
        assert!(compute("/zzz", &[]).is_none());
    }

    #[test]
    fn top_level_command_marks_children() {
        let s = compute("/mc", &[]).unwrap();
        let mcp = s.items.iter().find(|c| c.display == "/mcp").unwrap();
        assert!(mcp.has_children);
        assert_eq!(mcp.insert, "/mcp ");
        let help = compute("/hel", &[]).unwrap();
        let h = help.items.iter().find(|c| c.display == "/help").unwrap();
        assert!(!h.has_children);
    }

    #[test]
    fn skill_display_name_handles_paths_and_names() {
        assert_eq!(skill_display_name("/a/b/skills/foo/skill.yaml"), "foo");
        assert_eq!(skill_display_name("bar.yaml"), "bar");
        assert_eq!(skill_display_name("baz"), "baz");
    }

    #[test]
    fn multiline_input_has_no_menu() {
        assert!(compute("/mcp\nlist", &[]).is_none());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `ORT_STRATEGY=download cargo test -p mur-core complete`
Expected: FAIL — compile error, `Candidate` / `compute` / `skill_display_name` not found.

- [ ] **Step 4: Write the implementation**

Insert this ABOVE the `#[cfg(test)]` block in `complete.rs`:

```rust
use std::collections::HashSet;
use std::path::Path;

/// Most rows shown before the menu scrolls (kept in sync with `ui.rs`).
pub const MAX_MENU_ROWS: usize = 8;

/// One selectable menu entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// What the menu shows in the left column (`/skill`, `list`, `create-pr`).
    pub display: String,
    /// Text that replaces the whole input line on accept (`/mcp `, `/mcp list `,
    /// `create-pr`).
    pub insert: String,
    /// Right-column description (may be empty).
    pub desc: String,
    /// True for a top-level command that has a subcommand layer — accepting it
    /// keeps the menu open and shows layer 2.
    pub has_children: bool,
}

/// The live menu: the filtered candidates plus the highlighted row.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionState {
    pub items: Vec<Candidate>,
    pub selected: usize,
}

/// Built-in commands: (word without slash, description, subcommands).
/// `exit` is omitted as a duplicate of `quit`.
const COMMANDS: &[(&str, &str, &[&str])] = &[
    ("help", "show the command cheatsheet", &[]),
    ("clear", "start a new conversation", &[]),
    ("card", "show this agent's card", &[]),
    ("sessions", "list past sessions", &[]),
    ("channels", "list or switch channels", &[]),
    ("auto", "session-wide auto-approval", &["on", "off"]),
    (
        "mcp",
        "manage MCP servers",
        &["list", "add", "remove", "add-remote", "login", "registry-add"],
    ),
    ("skill", "manage agent skills", &["list", "add", "remove"]),
    ("skin", "switch theme", &["dark", "light", "mur"]),
    ("quit", "exit the chat", &[]),
];

/// Subcommands for `cmd` (without leading slash), or `None` if `cmd` is unknown
/// or takes only free-text args.
fn subcommands_for(cmd: &str) -> Option<&'static [&'static str]> {
    COMMANDS
        .iter()
        .find(|(w, _, _)| *w == cmd)
        .map(|(_, _, subs)| *subs)
        .filter(|subs| !subs.is_empty())
}

/// Top-level candidates: every built-in command plus the agent's skills.
fn build_top_level(skills: &[Candidate]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = COMMANDS
        .iter()
        .map(|(word, desc, subs)| Candidate {
            display: format!("/{word}"),
            insert: format!("/{word} "),
            desc: (*desc).to_string(),
            has_children: !subs.is_empty(),
        })
        .collect();
    out.extend_from_slice(skills);
    out
}

/// Layer-2 candidates for a command word.
fn build_subcommands(cmd: &str, subs: &[&str]) -> Vec<Candidate> {
    subs.iter()
        .map(|sub| Candidate {
            display: (*sub).to_string(),
            insert: format!("/{cmd} {sub} "),
            desc: String::new(),
            has_children: false,
        })
        .collect()
}

/// Case-insensitive substring filter on the candidate word (display minus any
/// leading `/`).
fn filter(cands: Vec<Candidate>, query: &str) -> Vec<Candidate> {
    let q = query.to_lowercase();
    cands
        .into_iter()
        .filter(|c| c.display.trim_start_matches('/').to_lowercase().contains(&q))
        .collect()
}

/// Derive the completion menu from the current input. Returns `None` when the
/// input is not in a slash context or nothing matches (menu closed).
pub fn compute(input: &str, skills: &[Candidate]) -> Option<CompletionState> {
    // ponytail: slash commands are single-line; a multiline composer has no menu.
    if input.contains('\n') {
        return None;
    }
    let after = input.trim_start().strip_prefix('/')?;
    let items = match after.split_once(char::is_whitespace) {
        // Still typing the command word.
        None => filter(build_top_level(skills), after),
        // Command word complete → maybe a subcommand layer.
        Some((cmd, rest)) => {
            // A second whitespace means we're typing an arg past layer 2.
            if rest.trim_start().contains(char::is_whitespace) {
                return None;
            }
            let subs = subcommands_for(cmd)?;
            filter(build_subcommands(cmd, subs), rest.trim_start())
        }
    };
    if items.is_empty() {
        return None;
    }
    Some(CompletionState { items, selected: 0 })
}

/// Best-effort display name for a skill source string: a path like
/// `.../skills/<name>/skill.yaml` → `<name>`; `<name>.yaml` → `<name>`;
/// a bare name → itself.
pub fn skill_display_name(raw: &str) -> String {
    let p = Path::new(raw);
    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
        if stem == "skill" {
            if let Some(parent) = p.parent().and_then(|d| d.file_name()).and_then(|s| s.to_str()) {
                return parent.to_string();
            }
        }
        return stem.to_string();
    }
    raw.to_string()
}

/// Load this agent's skills as menu candidates. Fail-soft: any read error
/// yields an empty list (the menu just shows built-in commands). Disabled
/// skills are excluded since they are not injected. ponytail: cached once at
/// startup; mid-session `/skill add` won't refresh it.
pub fn load_agent_skills(agent: &str) -> Vec<Candidate> {
    let Ok((_path, profile)) = crate::cmd::agent::load_profile_for_edit(agent) else {
        return Vec::new();
    };
    let disabled: HashSet<&str> = profile.disabled_skills.iter().map(String::as_str).collect();
    let mut out: Vec<Candidate> = Vec::new();
    for s in &profile.installed_skills {
        if disabled.contains(s.name.as_str()) {
            continue;
        }
        out.push(Candidate {
            display: s.name.clone(),
            insert: s.name.clone(),
            desc: s.description.clone(),
            has_children: false,
        });
    }
    for raw in &profile.skills {
        let name = skill_display_name(raw);
        if disabled.contains(name.as_str()) || out.iter().any(|c| c.display == name) {
            continue;
        }
        out.push(Candidate {
            display: name.clone(),
            insert: name,
            desc: String::new(),
            has_children: false,
        });
    }
    out
}
```

Note: `load_profile_for_edit` is `pub(crate)` in `crate::cmd::agent`; the full path above resolves from this module.

- [ ] **Step 5: Run tests to verify they pass**

Run: `ORT_STRATEGY=download cargo test -p mur-core complete`
Expected: PASS (all 12 tests).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/complete.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): pure completion module for agent cli autocomplete"
```

---

## Task 2: Wire menu state into `App`

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (struct fields ~line 284, `App::new` ~line 333, imports ~line 10)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (after `build_app` call, line 230)

**Interfaces:**
- Consumes: `complete::{Candidate, CompletionState, compute, load_agent_skills}` from Task 1.
- Produces: `App.completion: Option<CompletionState>` and `App.skills: Vec<Candidate>` (used by Tasks 3–4).

- [ ] **Step 1: Import the completion types in `app.rs`**

Add to the `use super::...` block near the top of `app.rs` (after `use super::welcome::...` at line ~16):

```rust
use super::complete::{Candidate, CompletionState};
```

- [ ] **Step 2: Add the fields to the `App` struct**

In `app.rs`, immediately after `pub auto_reads: bool,` (line ~284, the last field before the closing `}`):

```rust
    /// Live completion menu (slash commands / agent skills). `None` = closed.
    /// Derived from the input text — recomputed on every edit by `mod.rs`.
    pub completion: Option<CompletionState>,
    /// This agent's skills as menu candidates, loaded once at startup.
    pub skills: Vec<Candidate>,
```

- [ ] **Step 3: Initialise the fields in `App::new`**

In `app.rs`, immediately after `auto_reads: false,` (line ~333, before the closing `}` of the `Self { ... }`):

```rust
            completion: None,
            skills: Vec::new(),
```

- [ ] **Step 4: Verify it compiles**

Run: `ORT_STRATEGY=download cargo check -p mur-core`
Expected: PASS (no warnings about the new fields; `skills`/`completion` are read in later tasks — if clippy flags dead_code at this point it is expected and cleared by Task 3/4).

- [ ] **Step 5: Load the skills at startup**

In `mur-core/src/cmd/agent/cli/mod.rs`, immediately after line 230 `let mut app = build_app(&home, &agent, resume, active_theme)?;`:

```rust
    app.skills = complete::load_agent_skills(&agent);
```

- [ ] **Step 6: Verify it compiles**

Run: `ORT_STRATEGY=download cargo check -p mur-core`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): hold completion menu + agent skills on App"
```

---

## Task 3: Key handling — open, navigate, accept, dismiss

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (key match ~line 366–424, paste arm ~line 439, `complete_slash` ~line 575–588)

**Interfaces:**
- Consumes: `App.completion`, `App.skills`, `complete::compute`, `App::set_input`, `App::input_text`.
- Produces: three free fns `refresh_completion`, `completion_move`, `completion_accept`; deletes `complete_slash`.

- [ ] **Step 1: Add the menu-key interceptor before the main key match**

In `handle_event`, between `let alt = key.modifiers.contains(KeyModifiers::ALT);` (line 366) and `match key.code {` (line 367), insert:

```rust
            // While the completion menu is open it owns navigation / accept /
            // dismiss keys; everything else falls through to normal editing and
            // re-filters the menu at the end of this handler.
            if app.completion.is_some() {
                match key.code {
                    KeyCode::Up => {
                        completion_move(app, -1);
                        return;
                    }
                    KeyCode::Down => {
                        completion_move(app, 1);
                        return;
                    }
                    KeyCode::Char('p') if ctrl => {
                        completion_move(app, -1);
                        return;
                    }
                    KeyCode::Char('n') if ctrl => {
                        completion_move(app, 1);
                        return;
                    }
                    KeyCode::Tab | KeyCode::Enter => {
                        completion_accept(app);
                        return;
                    }
                    KeyCode::Esc => {
                        app.completion = None;
                        return;
                    }
                    _ => {}
                }
            }
```

- [ ] **Step 2: Change the closed-menu Tab arm to open the menu**

In the main `match key.code`, replace line 388 `KeyCode::Tab => complete_slash(app),` with:

```rust
                KeyCode::Tab => refresh_completion(app),
```

- [ ] **Step 3: Refresh the menu after every key edit**

At the end of the `Event::Key(...)` arm — immediately after the inner `match key.code { ... }` closes (the `}` at line ~424) and before the arm's own closing `}` — add:

```rust
            refresh_completion(app);
```

- [ ] **Step 4: Refresh the menu after a paste**

In the `Event::Paste(text)` arm, after the closing `}` of the `if/else if/else` that inserts the text (after line 440), add:

```rust
            refresh_completion(app);
```

- [ ] **Step 5: Replace `complete_slash` with the three new functions**

Replace the whole `complete_slash` function (lines 575–588) with:

```rust
/// Recompute the completion menu from the current input. Called after every
/// edit and when Tab is pressed with the menu closed.
fn refresh_completion(app: &mut App) {
    app.completion = complete::compute(&app.input_text(), &app.skills);
}

/// Move the highlighted row by `delta`, wrapping.
fn completion_move(app: &mut App, delta: isize) {
    if let Some(c) = &mut app.completion {
        let n = c.items.len() as isize;
        if n == 0 {
            return;
        }
        c.selected = (c.selected as isize + delta).rem_euclid(n) as usize;
    }
}

/// Accept the highlighted candidate: replace the input line with its insert
/// text. A command with a subcommand layer keeps the menu open (now showing
/// layer 2); everything else closes it.
fn completion_accept(app: &mut App) {
    let Some(c) = app.completion.as_ref() else {
        return;
    };
    let Some(cand) = c.items.get(c.selected) else {
        app.completion = None;
        return;
    };
    let insert = cand.insert.clone();
    let descend = cand.has_children;
    app.set_input(&insert);
    app.completion = if descend {
        complete::compute(&app.input_text(), &app.skills)
    } else {
        None
    };
}
```

- [ ] **Step 6: Verify it compiles (no leftover `complete_slash` references)**

Run: `ORT_STRATEGY=download cargo check -p mur-core`
Expected: PASS, no "cannot find function `complete_slash`" and no dead-code warning for it.

- [ ] **Step 7: Run the full module + a clippy pass**

Run: `ORT_STRATEGY=download cargo test -p mur-core complete && cargo clippy -p mur-core -- -D warnings`
Expected: tests PASS, clippy clean.

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): open/navigate/accept the agent cli completion menu with Tab"
```

---

## Task 4: Render the popup overlay (`ui.rs`)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/ui.rs` (imports line 7, `render` line 16–39, new `render_completion` fn)

**Interfaces:**
- Consumes: `App.completion`, `complete::{CompletionState, MAX_MENU_ROWS}`, `app.theme`.

- [ ] **Step 1: Extend the widget imports**

In `ui.rs`, change the widgets import (line 7) to add `List`, `ListItem`, `ListState`:

```rust
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap};
```

- [ ] **Step 2: Call the overlay renderer from `render`**

In `render`, after `f.render_widget(&app.input, chunks[1]);` (line 29), add:

```rust
    render_completion(f, app, chunks[1]);
```

- [ ] **Step 3: Implement `render_completion`**

Add this function to `ui.rs` (e.g. after `render`):

```rust
/// Draw the completion menu as a floating list anchored just above the input
/// box. No-op when the menu is closed or empty.
fn render_completion(f: &mut Frame, app: &App, input_area: Rect) {
    let Some(state) = &app.completion else {
        return;
    };
    if state.items.is_empty() {
        return;
    }
    let theme = app.theme;

    let rows: Vec<ListItem> = state
        .items
        .iter()
        .map(|c| {
            let mut spans = vec![Span::styled(
                c.display.clone(),
                Style::default().fg(theme.border_title),
            )];
            if !c.desc.is_empty() {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    c.desc.clone(),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let visible = state.items.len().min(complete::MAX_MENU_ROWS) as u16;
    let height = visible + 2; // borders
    let width = input_area.width;
    // Sit directly above the input; clamp to the top of the frame if cramped.
    let y = input_area.y.saturating_sub(height).max(f.area().y);
    let area = Rect {
        x: input_area.x,
        y,
        width,
        height,
    };

    let list = List::new(rows)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.border))
                .title(" ↑↓ move · Tab accept · Esc close ")
                .title_style(Style::default().fg(theme.border_title)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected));

    f.render_widget(Clear, area);
    f.render_stateful_widget(list, area, &mut list_state);
}
```

Add `use super::complete;` to the `use super::...` block at the top of `ui.rs` if not already imported.

- [ ] **Step 4: Verify it compiles**

Run: `ORT_STRATEGY=download cargo check -p mur-core`
Expected: PASS.

- [ ] **Step 5: Manual smoke test (TUI can't be unit-tested)**

Run a debug build against a real agent and confirm: typing `/` opens the menu; `/sk` filters to `/skill`/`/skin`; `↑↓` move; `Tab` on `/mcp` shows `list/add/remove/...`; `Tab` on a leaf inserts `/mcp list `; `Esc` closes; a matching skill name appears and accepting it inserts the bare name.

Run: `ORT_STRATEGY=download cargo run -p mur-core -- agent cli <some-agent>`
Expected: behavior above. (If no agent exists, `mur agent create` one first, or test against `mur`.)

- [ ] **Step 6: Format, lint, full check**

Run: `cargo fmt -p mur-core && cargo clippy -p mur-core -- -D warnings && ORT_STRATEGY=download cargo test -p mur-core complete`
Expected: fmt clean, clippy clean, tests PASS.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(cli): render the agent cli completion menu overlay"
```

---

## Self-Review notes (already reconciled)

- **Spec coverage:** filtered popup menu (Task 4) · `/` auto-open + Tab/Enter accept + ↑↓ + Esc (Task 3) · built-in commands with layer-2 subcommands and agent skills (Task 1) · skill accept = bare-name soft-invoke (Task 1 `Candidate.insert`) · no runtime/A2A change (single crate) · Type 2 / `@`-files / fuzzy / real skill invocation explicitly out of scope.
- **Type consistency:** `Candidate`/`CompletionState`/`compute`/`load_agent_skills`/`MAX_MENU_ROWS`/`skill_display_name` are defined in Task 1 and referenced with those exact names in Tasks 2–4. `refresh_completion`/`completion_move`/`completion_accept` are defined and called within Task 3.
- **Placeholder scan:** no TBD/TODO; every code step shows full code.
- **Known fail-soft seams (intentional):** `load_agent_skills` returns empty on any profile read error; multiline input disables the menu; legacy `profile.skills` names are best-effort via `skill_display_name`.
