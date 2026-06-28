# `mur agent cli` autocomplete (Type 1: completion menu)

**Date:** 2026-06-28
**Status:** Design approved, pending implementation plan
**Scope:** `mur agent cli` interactive TUI (`murmur`). Type 1 only — the
filtered completion menu over slash commands + agent skills. Type 2
(agent-suggested ghost text) is explicitly deferred to a follow-on spec.

## Goal

Replace the current one-shot `complete_slash()` (prefix-match, first hit,
whole-buffer replace) with a Claude-Code-style filtered completion menu:

- Typing `/` opens a floating menu of everything the user can invoke.
- The menu lists **built-in slash commands** (shown with `/`) and **this
  agent's skills** (shown bare).
- It filters as you type, supports a second layer for commands that have
  subcommands, and accepts with `Tab`/`Enter`.

This is a single-crate change to `mur-core`. It does **not** touch the agent
runtime, the A2A envelope, or `mur-common`.

## Background (current code)

- TUI: `ratatui` 0.29 + `crossterm` + `tui_textarea`.
  - Event loop: `mur-core/src/cmd/agent/cli/mod.rs:299`
  - Key match: `mod.rs:367`; `Tab` → `complete_slash()` at `mod.rs:388/576`
  - Input widget: `App.input: TextArea<'static>` (`app.rs:203`), rendered at
    `ui.rs:29`
- Slash model already has layers: `SlashCmd` enum (`app.rs:102`) carries
  subcommands — `/mcp list|add|remove|add-remote|login|registry-add`,
  `/skill list|add|remove`, `/auto on|off`, `/skin dark|light|mur`,
  `/channels <N>`. `SLASH_COMMANDS` static list at `app.rs:149`.
- Agent skills: `profile.skills` + `profile.installed_skills`
  (`SkillCardEntry { name, description, ... }`) in
  `~/.mur/agents/<name>/profile.yaml`, read via `load_profile_for_edit()`
  (blocking I/O).
- No ghost-text / suggestion rendering today; placeholder is static.

## Decisions

1. **Skill accept = insert bare name as message text (soft-invoke).** MUR
   skills are auto-injected knowledge objects, not invocable commands (unlike
   Claude Code, where a skill *is* a slash command that runs on Enter). Within
   the Type-1-only scope, accepting a skill inserts its bare name (e.g.
   `create-pr`) into the message; on `Enter` the normal retrieve/inject
   pipeline plus the literal phrase bring the capability in. No runtime change.
   `// ponytail:` comment marks this; real invocation (`/skill run <name>`) is
   a future option, not v1.
2. **`/` auto-opens the menu** (not Tab-only). `Tab`/`Enter` accept; `↑↓`
   (and `Ctrl+P`/`Ctrl+N`) move; `Esc` closes. When the menu is closed,
   `Enter` submits as today.
3. **Filtered popup list** (ratatui `List` overlay), not in-place cycling —
   so per-agent skills can be shown with descriptions.

## UX

```
┌─ message ──────────────────────────────┐
│ /sk▏                                    │
└─────────────────────────────────────────┘
 ╭─────────────────────────────────────────╮
 │ /skill       manage agent skills        │
 │ /skin        switch theme               │
 │ create-pr    open a PR for the diff   ◀ skill
 │ update-pr    amend the open PR          │
 ╰─────────────────────────────────────────╯
  ↑↓ move · Tab accept · Esc close
```

- **Open:** the current `/`-token at the cursor begins with `/`. Menu refreshes
  on every keystroke. If the token stops starting with `/`, the menu closes.
- **Filter:** case-insensitive substring of the `/`-token (minus the leading
  `/`) against each candidate's display name. (Substring, not fuzzy — cheap and
  predictable; fuzzy ranking is out of scope.)
- **Accept (`Tab`/`Enter`):**
  - Built-in command → insert `/<cmd> `. If it has subcommands, the menu
    **stays open at layer 2** showing those subcommands. Accepting a leaf
    subcommand inserts it and closes the menu.
  - Skill → insert bare `<name>`, close the menu.
- **Navigate:** `↑`/`↓` or `Ctrl+P`/`Ctrl+N`; selection wraps. List caps at
  ~8 visible rows and scrolls.
- **Dismiss:** `Esc` closes the menu, leaving the typed text untouched.
- **Submit:** `Enter` with the menu closed submits the line (unchanged).

### Layers

Layer 2 comes from a **static command→subcommands table** — no runtime lookup:

| Command     | Subcommands (layer 2)                                  |
|-------------|--------------------------------------------------------|
| `/mcp`      | `list`, `add`, `remove`, `add-remote`, `login`, `registry-add` |
| `/skill`    | `list`, `add`, `remove`                                |
| `/auto`     | `on`, `off`                                            |
| `/skin`     | `dark`, `light`, `mur`                                 |
| `/channels` | *(numeric arg — no completion)*                        |

Skills are flat (1 layer). This satisfies the requested 1-layer
(`create-pr`) / 2-layer (`/mcp list`) behavior.

## Architecture

New module **`mur-core/src/cmd/agent/cli/complete.rs`** — pure data + pure
functions, so it unit-tests without a TUI and keeps `mod.rs` under the 800-line
rule.

```rust
enum CandidateKind { Command, Subcommand, Skill }

struct Candidate {
    display: String,      // shown in the menu ("/skill", "create-pr")
    insert: String,       // text inserted on accept ("/skill ", "create-pr")
    desc: String,         // right-hand description
    kind: CandidateKind,
    children: Vec<Candidate>, // layer-2 subcommands (empty for leaves/skills)
}

struct CompletionState {
    items: Vec<Candidate>,   // filtered candidates for the current layer
    selected: usize,
    layer: Layer,            // TopLevel | Sub(command)
    anchor: usize,           // byte offset where the `/`-token starts
}
```

Functions (all pure / synchronous):
- `build_top_level(skills: &[Candidate]) -> Vec<Candidate>` — static command
  table + the agent's skill candidates.
- `filter(candidates, query) -> Vec<Candidate>` — case-insensitive substring.
- `accept(state, selected) -> Accept` — returns the insert string and whether
  to descend to layer 2 or close.

**App changes** (`app.rs`):
- `completion: Option<CompletionState>`
- `skills: Vec<Candidate>` — built **once at App construction** from
  `load_profile_for_edit(&agent)` (`profile.skills` + `installed_skills`).
  Cached to avoid blocking I/O on every keystroke; staleness within a session
  is acceptable (`// ponytail:` — refresh-on-`/skill add` deferred).

**`mod.rs` changes:**
- After any input edit, recompute the `/`-token at the cursor and open / refresh
  / close `app.completion`.
- When `completion.is_some()`, intercept `Tab`/`Enter`/`↑↓`/`Ctrl+P`/`Ctrl+N`/
  `Esc` for the menu; otherwise fall through to existing handlers.
- Delete the old `complete_slash()`.

**`ui.rs` changes:**
- When `app.completion` is `Some`, compute a popup `Rect` anchored to the input
  box (above it, flipping below if no room), sized to the visible candidate
  count, and render a `List` with the selected row highlighted. Drawn after the
  input so it overlays.

## Out of scope (future specs)

- **Type 2 — agent-suggested ghost text.** Separate spec: add an optional
  `suggestions: Vec<String>` to the A2A reply envelope (`mur-common`); the
  runtime populates it; the TUI renders the first as greyed inline ghost text in
  the empty input and `Tab` inserts it. Cross-crate; deliberately not in v1.
- `@`-file-path completion.
- Fuzzy ranking of candidates.
- Real per-skill invocation (`/skill run <name>`) — would need runtime support.

## Testing

`complete.rs` `#[cfg(test)]` (no TUI needed — pure functions):
- `filter` is case-insensitive substring and matches both commands and skills.
- Accepting `/mcp` descends to layer 2 with the right subcommands; accepting a
  leaf subcommand closes and inserts `"/mcp list "`.
- Accepting a skill inserts the bare name and closes.
- Typing past a non-`/` token closes the menu (token detection).
