# Post-update stale interactive CLI notice

**Status:** Approved in conversation 2026-09-14; implementation pending.
**Scope:** `mur-core/src/update/` only. The notice is best-effort and does not alter binary replacement, agent restart, daemon restart, or update exit status.

## Problem

On Unix, `mur update` atomically replaces the executable pathname while the process that invoked it continues to execute the old mapped image. `mur update --restart-agents` restarts managed agents (and the daemon), but cannot replace an interactive `mur` or `murmur` process already in a terminal. A user can therefore successfully update, run a job in that same terminal, and unknowingly exercise old behavior.

## Decision

After a successful Unix binary swap and post-upgrade work, inspect running processes best-effort and report interactive MUR processes whose executable is the just-replaced binary but whose process ID is not the updater itself.

The updater itself is always reported separately because it necessarily retains the old image after the swap. Other matching processes are listed as `mur` or `murmur` sessions with PID and executable path. Background binaries (`murmurd`, `mur-agent-runtime`, MCP server, research gateway) are excluded: `--restart-agents` already owns agent and daemon restarts, and the notice must not duplicate that status.

The report is Unix-only. Windows stages replacement until process exit, so this particular inode/image split does not exist at report time. Failure to enumerate processes or obtain their executable paths yields no notice and never converts an otherwise successful update into failure.

## Interface

`update::stale_interactive_sessions(processes, updater_pid, replaced_executable)` is a pure helper returning sorted session records. Its inputs are lightweight process snapshots so tests do not read the host process table. A process qualifies only when:

1. its PID differs from the updater;
2. its executable path equals the replaced executable path; and
3. its basename is `mur` or `murmur` (with `.exe` accepted for cross-platform test data).

`warn_stale_interactive_sessions(replaced_executable)` obtains snapshots through `sysinfo` and prints the report. It is called only after a successful Unix swap.

## Output

```text
⚠ This terminal is still running the previous MUR binary. Close and reopen it before starting jobs.
ℹ Other interactive MUR sessions still running the previous binary:
  • mur (PID 4128, /opt/homebrew/bin/mur)
```

The first line is always printed after an upgrade. The second block is printed only if other qualifying sessions exist. It deliberately names no version: a process table exposes executable path reliably, whereas inferring a process's embedded package version would require launching or inspecting a possibly deleted mapped image.

## Tests

Unit tests cover: the current updater is excluded from the list but still receives its dedicated notice at the caller; only `mur`/`murmur` with the replaced executable qualify; agents and daemons are excluded; different executable paths are excluded; and output ordering is deterministic. Existing update tests remain unchanged.
