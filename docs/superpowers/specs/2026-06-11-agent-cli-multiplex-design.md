# Agent CLI Multiplex + `murmur` Quick Command — Design

**Date:** 2026-06-11
**Status:** Approved (brainstorming session)
**Scope:** `mur agent cli` multi-agent split windows via external multiplexer orchestration, plus a `murmur` quick-command symlink.

## Problem

1. `mur agent cli <name>` only opens one chat per shell window. Running several agents side by side requires manually opening terminals and typing the full command in each.
2. `mur agent cli <name>` is too long for a command users type many times a day.

## Decisions (from brainstorming)

- Each chat pane is a **full independent terminal** (own scrollback, copy/paste, detach), and the user may open more than 4 at once → external multiplexer orchestration, not an in-process ratatui multi-pane view.
- **tmux is the primary backend** (the claude-squad precedent), with auto-detection of zellij / WezTerm / kitty when the user is already inside one of them.
- The quick command is **`murmur`** — a symlink to `mur` dispatched via argv[0] (same BusyBox-style convention as `mur_agent_<name>` → `mur-agent-runtime`). Brand fit: agents are "murmur agents".
- In-process PTY embedding (tui-term + portable-pty) was researched and **rejected**: pre-1.0, nested alternate-screen rendering is fragile.

## Design

### 1. CLI surface

- `mur agent cli <name>...` accepts **one or more** agent names.
  - One name: existing behavior, byte-for-byte unchanged (single TUI in current terminal).
  - Two or more names: orchestration mode — spawn one pane per agent, each pane running single-name `mur agent cli <name>`.
- Flags `--resume` / `--auto` remain valid and are forwarded to every spawned pane.
- **`murmur`** symlink → `mur`. `main.rs` inspects argv[0]; when the file stem is `murmur`, the remaining args are parsed as `mur agent cli <args>`:
  - `murmur a1` ≡ `mur agent cli a1`
  - `murmur a1 a2 a3` ≡ `mur agent cli a1 a2 a3`
  - `murmur --resume a1` ≡ `mur agent cli --resume a1`
  - `murmur` (no args): if the concierge agent `mur` exists, open a chat with it; otherwise list available agents and exit non-zero.

### 2. Orchestration backend detection

New module `mur-core/src/cmd/agent/cli/multiplex.rs` (respecting the ≤800-line rule). Detection order, first match wins:

| # | Condition | Action |
|---|-----------|--------|
| 1 | `$TMUX` set (inside tmux) | `tmux new-window` in the current session, then `split-window` N−1 times, `select-layout tiled`. The user's current window is untouched. |
| 2 | `$ZELLIJ` set (inside zellij) | `zellij action new-tab`, then one `zellij run -- mur agent cli <name>` per agent. |
| 3 | `$WEZTERM_PANE` set | `wezterm cli split-pane` per agent, alternating direction for a rough grid. |
| 4 | `$KITTY_WINDOW_ID` set | `kitten @ launch --location=vsplit` per agent. If remote control is disabled (command fails), fall through to row 5. |
| 5 | `tmux` on PATH | `tmux new-session -d -s mur-chat` (suffix `-2`, `-3`… on collision), split N panes, `select-layout tiled`, `attach`. |
| 6 | `zellij` on PATH | `zellij --layout-string` with a generated KDL layout of N command panes. |
| 7 | none | Error: explain that multi-agent split needs a multiplexer; suggest `brew install tmux`. |

Each pane command is the absolute path of the current executable (`std::env::current_exe()`) with `agent cli <single-name> [flags]` — no dependence on PATH or the `murmur` symlink inside panes.

### 3. Error handling

- **Validate before spawning:** canonicalize every name via `a2a_dial::canonicalize_agent_name` and check the agent profile exists. Any invalid name aborts the whole batch with a list of unknown names — never open panes that immediately show errors.
- Duplicate names are allowed (two independent conversations with the same agent are legitimate). `--resume` with duplicates resumes the same session in each pane; documented as-is.
- External command failure (tmux exits non-zero, kitty refuses remote control) → clear message naming the backend that failed, then fall through to the next detection row where the table says so (kitty→5); otherwise abort with the failure output.

### 4. Install / packaging

- `install.sh` and `build.sh --install`: after installing `mur`, create/refresh the `murmur` symlink next to it.
- Homebrew formula (release workflow): add `bin.install_symlink` for `murmur`.
- Docs to update per the documentation checklist: `README.md`, app.mur.run docs-content + navigation, `docs/architecture/runtime-overview.md` CLI surface section, and the `CLAUDE.md` CLI surface line.

### 5. Testing

- Unit tests (no TTY needed):
  - Backend detection with injected env vars (`$TMUX`, `$ZELLIJ`, `$WEZTERM_PANE`, `$KITTY_WINDOW_ID`) and a fake PATH probe.
  - Generated argv vectors for each backend (tmux split sequence, zellij KDL layout string, wezterm/kitty invocations) — pure functions returning `Vec<Vec<String>>` asserted exactly.
  - Name validation: batch abort on unknown agent; duplicates accepted.
  - argv[0] dispatch: `murmur` file stem maps args to `agent cli`, no-arg concierge fallback.
- Manual E2E: real tmux/zellij/WezTerm/kitty sessions (CI has no TTY). Checklist included in the implementation plan.

## Non-goals

- In-process multi-pane ratatui rendering (rejected; revisit if tui-term reaches 1.0).
- Windows support for orchestration mode (tmux/zellij only; single-name mode unaffected).
- Pane lifecycle management after spawn (closing one chat does not affect siblings; no session GC).

## Research references

- claude-squad (tmux orchestration precedent): https://github.com/smtg-ai/claude-squad
- zellij CLI control / `--layout-string` (v0.44.1+): https://zellij.dev/documentation/controlling-zellij-through-cli
- WezTerm CLI: https://wezterm.org/
- kitty remote control: https://sw.kovidgoyal.net/kitty/remote-control/
- tui-term (evaluated, rejected): https://github.com/a-kenji/tui-term
