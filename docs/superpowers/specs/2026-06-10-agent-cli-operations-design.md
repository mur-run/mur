# `mur agent cli` Operations Fixes — Design

Date: 2026-06-10. Status: approved.

Live-testing the TUI (`mur agent cli mur`) via tmux surfaced 9 issues. Fix in three
batches. Findings log: `/tmp/mur-cli-test-ops.log`.

## Issues

| # | Issue | Root cause (where known) |
|---|-------|--------------------------|
| 1 | No auto-approve permission mode | HITL modal offers only y/n |
| 2 | YouTube analysis fails | GUI-launched runtime PATH lacks `/opt/homebrew/bin`; `detect_ytdlp()` only searches PATH → `MediaError::YtdlpMissing` |
| 3 | `!cmd` shell escape missing | Sent to agent as chat text |
| 4 | `/mcp` slash command missing | — |
| 5 | `/skill` slash command missing | — |
| 6 | **After HITL approve, the turn is lost**: spinner freezes, final reply never renders, tool result absent from context, session JSONL has no agent turn | TBD — diagnose runtime resume path vs TUI stream handling |
| 7 | Keystrokes during HITL modal silently swallowed | Modal drops all non-y/n keys |
| 8 | Hub.app bundles stale runtime: skill loader rejects `category: media` | Repo already has `Category::Media`; redeploy needed (ops, not code) |
| 9 | `stderr.log` spammed with "Operation not permitted" exec failures | Sidecar respawn loop execing from `/Volumes` (macOS blocks); investigate |

## Batches

- **Batch 1 (HITL/permissions core):** #6 bug fix, #1 auto mode, #7 input buffering
- **Batch 2 (toolchain/env):** #2 tool-detection fallback dirs, #3 `!` escape, #8+#9 deploy/investigation
- **Batch 3 (TUI management):** #4 `/mcp`, #5 `/skill`, help/tab-completion updates

## Designs

### Auto permission mode (#1)

Modeled on Claude Code permission modes, adapted to MUR's HITL protocol:

- HITL modal gains a third key: `[y]` allow once · `[a]` always allow **this tool**
  for this session · `[n]` deny. Session allowlist lives in TUI `App` state; matching
  requests auto-approve with a `· auto-approved <tool> (session)` notice.
- `/auto` slash command + `--auto` CLI flag: session-scoped approve-all. Status bar
  shows a warning-colored `AUTO` tag. `/auto off` disables. Never persisted —
  restart returns to ask-first. Persistent grants stay in `mur agent perm`.

### `!` shell escape (#3)

`!cmd` runs locally via `$SHELL -c` (30 s timeout, output truncated ~100 lines/8 KB),
renders as a distinct bubble, persists to the session JSONL, and accumulated
command+output blocks are prefixed to the next user message so the agent sees them
(Claude Code behavior).

### `/mcp`, `/skill` (#4, #5)

Reuse `cmd/agent/mcp.rs` / `skill.rs` logic in-process: `/mcp` list|add|remove,
`/skill` list|install|remove. After mutation, notify that the agent needs a restart
to pick up profile changes (unless runtime hot-reloads — verify).

### Tool detection fallback (#2)

`detect_tool()` falls back to well-known dirs (`/opt/homebrew/bin`, `/usr/local/bin`,
`~/.local/bin`) after PATH search fails. Fixes every GUI-launched runtime, not just
yt-dlp (ffmpeg too).
