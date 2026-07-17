# rust-analyzer Memory Reduction — Design

**Date:** 2026-07-17
**Status:** Implemented (2026-07-17)

## Implementation record

- `~/.claude/settings.json`: `"rust-analyzer-lsp@claude-plugins-official": false`
  (more targeted than the spec's `CLAUDE_CODE_DISABLE_LSP=1` — swift-lsp/php-lsp
  stay enabled).
- `~/.config/rust-analyzer/rust-analyzer.toml`: written as specced.
- `~/Library/LaunchAgents/com.david.rust-analyzer-reaper.plist` +
  `~/.local/bin/rust-analyzer-reaper.sh`: loaded via `launchctl bootstrap`;
  match logic verified against live processes (no false positives).
- Pre-existing instances keep the old settings until their sessions end; new
  sessions get one lean rust-analyzer (Serena's) only.
- First symbol query per session is now slow (seconds) by design — cache
  priming is off, not a hang.

## Problem

Each Claude Code session working on the mur repo spawns up to two rust-analyzer
instances (~1–3 GB each on this workspace):

1. The Serena MCP server spawns one for its symbol tools.
2. Claude Code's built-in LSP tool spawns another (duplicate capability).

With 3 worktrees open, that is ~10 GB. Instances also linger after sessions
end (orphaned, PPID=1) and stay fat while sessions idle.

## Decisions

- Keep **Serena** as the single LSP source; disable Claude Code's built-in LSP.
- Trade cold-query latency for memory (disable cache priming, cap LRU).
- Reap orphaned processes with a small LaunchAgent; do not touch live sessions.

## Design (three independent changes)

### 1. Disable built-in LSP (removes the duplicate)

`~/.claude/settings.json`:

```json
"env": { "CLAUDE_CODE_DISABLE_LSP": "1" }
```

Global scope. Built-in goToDefinition/findReferences disappear; Serena's
equivalent tools remain.

### 2. User-level rust-analyzer config (shrinks each instance)

`~/.config/rust-analyzer/rust-analyzer.toml`:

```toml
cachePriming.enable = false   # no upfront whole-workspace indexing into RAM
lru.capacity = 64             # query-cache cap (default 128)
```

User-level config applies to every project, worktree, and client. Expected:
~3 GB → ~1 GB per instance. Cost: first symbol query takes seconds instead of
being instant; subsequent queries are unaffected. Idle sessions stay small
because nothing is primed.

### 3. Orphan reaper (LaunchAgent)

A LaunchAgent runs every 5 minutes and kills only `rust-analyzer` /
`rust-analyzer-proc-macro-srv` processes whose parent is dead (PPID == 1).
Live sessions are never touched. Worst-case orphan lifetime: 5 minutes.

Files:
- `~/Library/LaunchAgents/com.david.rust-analyzer-reaper.plist`
- reaper is a one-liner (`ps` + `awk` + `kill`) embedded in the plist or a
  tiny script in `~/.local/bin/`

## Out of scope

- Serena configuration changes.
- Any file inside the mur repo (config is user-level).
- Idle-timeout monitoring of live instances.

## Rollback

Each change reverts independently: remove the env line / delete the toml /
`launchctl bootout` the agent.

## Verification

1. Start a Claude Code session in the repo → `ps` shows exactly one
   rust-analyzer (child of Serena), none child of `claude`.
2. After a Serena symbol query, RSS of rust-analyzer stays well under 3 GB.
3. Kill a `claude` process ungracefully → orphaned rust-analyzer disappears
   within 5 minutes.
