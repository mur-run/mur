# Murmur Panel P2 — Data Tabs + Schedule

**Date:** 2026-07-06
**Status:** Approved (brainstormed with David)
**Parent:** docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md (P1 skeleton, merged PR #629)

## Scope

P1 shipped the end-to-end skeleton with four empty tabs. P2 fills the
data tabs and adds a fifth tab, **Schedule**. Tab layout:

**Information / Activities / Preview (untouched, P3) / Notifications / Schedule**

Decisions from brainstorm:

- Schedule gets its own tab; cost merges into Information; harvest
  proposals merge into Notifications (option A — keep tab count at 5).
- Schedule is a **unified view over all three scheduler subsystems**,
  not just agent schedules: agent cron/idle triggers, workflow OS
  schedules (launchd/crontab), and fleet `loop.trigger`.
- Data is aggregated by a new hidden CLI command
  `mur internals schedule-status --json` (same pattern as the planned
  `mur internals recommend`), because the three schedule sources and
  their interpretation logic all live in mur-core / mur-agent-runtime.
  The Hub shells out and renders; no second interpretation of cron
  semantics appears Hub-side.

## Tab Contents & Data Sources

### 1. Information (Hub-side)

- **Git**: repo root, current branch, branch list, worktrees, dirty
  status — the Hub shells out to `git` against the session's cwd,
  refreshed on `state` frames and window focus (per the P1 spec).
- **Cost** (merged in): cumulative token usage / estimated spend for
  the bound agent's current channel, summed from `Task.usage` on
  channel events (`work.rs` already reads channels; add the fold).
  When the agent belongs to a fleet with a budget, show remaining
  budget.

### 2. Activities

- Running workflow runs, pending HITL gates, queued agent jobs —
  reuse the Hub's existing `work.rs` (channel summaries) and
  `hitl.rs`, filtered to the bound session's agent.
- Clicking a HITL item sends `insert` with the corresponding
  `mur channel approve <channel_id> <hitl_id>` command (insert-only;
  nothing executes without the user pressing Enter in murmur).

### 3. Notifications

- Existing `notif.rs` / EventBus event streams filtered to the bound
  agent.
- **Harvest proposals** (merged in): pending workflow-proposal count
  plus the newest few entries, read from
  `~/.mur/inbox/workflow-proposals/`. Clicking inserts
  `mur session out`.

### 4. Schedule (new tab)

Unified schedule view across three sources:

| Source | Storage | Executor |
|---|---|---|
| Agent cron / idle triggers | `profile.yaml` `lifecycle.schedule` + idle triggers | `mur-agent-runtime` supervisor tokio loops |
| Workflow schedules | launchd plist `com.mur.schedule.<name>.plist` (macOS) / crontab (Linux) | OS scheduler → `mur workflow run` (`cmd/system_schedule.rs`) |
| Fleet `loop.trigger` | `~/.mur/fleets/<name>/fleet.yaml` + `.last_run` / `.stopped` | daemon `fleet_tick` (requires `MUR_FLEET_AUTORUN=1`) |

Each row shows: kind, owner name, cron/interval expression, next 1–3
fire times, and status (enabled / stopped / budget remaining; for
fleets also whether `MUR_FLEET_AUTORUN` is set).

Default filter: entries related to the bound agent plus global
entries (workflows, fleets); a toggle switches to "all".

## New CLI: `mur internals schedule-status --json`

Hidden command in mur-core aggregating the three sources. Output:

```json
{ "schedules": [
  { "kind": "agent_cron", "owner": "mur", "expr": "30 8 * * 1-5",
    "message": "…", "next_fires": ["2026-07-07T08:30:00+08:00"], "status": "enabled" },
  { "kind": "agent_idle", "owner": "mur", "after_secs": 3600,
    "cooldown_secs": 600, "status": "enabled" },
  { "kind": "workflow", "owner": "daily-report", "expr": "0 9 * * *",
    "next_fires": ["…"], "status": "enabled" },
  { "kind": "fleet", "owner": "skillsmith", "trigger": "cron:0 3 * * *",
    "next_fires": ["…"], "status": "stopped", "budget_usd": 5.0,
    "autorun_env": false }
],
  "warnings": [] }
```

- Next-fire computation reuses
  `mur_agent_runtime::scheduler::next_n_fires`.
- Workflow cron expressions are read back from the installed launchd
  plist / crontab line (`list_system_schedules` exists; add cron
  expression extraction).
- `--agent <name>` filters to one agent's entries plus globals.
- **Fail-soft**: if any one source fails to read, that source
  contributes an empty list and a message in `warnings`; the command
  never fails as a whole, so the Panel always renders the remaining
  sources.

## Data Flow & Refresh

- All P2 data is Hub-side: Tauri command → read files / shell out →
  React render, following the existing `work.rs` pattern.
- **murmur (TUI) side needs zero changes** — P2 adds no new frames;
  `PanelTab` gains a `Schedule` variant only where the Hub renders
  tabs (the TUI's `/panel schedule` completion may land with it, but
  the wire enum change is additive and tolerated by the P1
  unknown-variant decoding).
- Refresh triggers: window focus, `state` frame, tab switch; Schedule
  and Activities additionally poll every 30 s while the window is
  visible.

## Testing

- `schedule-status` integration test: fixtures for all three sources
  plus a fail-soft case (one source unreadable → warnings populated,
  others intact).
- Cost fold unit test (`Task.usage` summation across delegate turns).
- Proposals directory read test (empty dir, N entries, malformed file
  skipped).
- Panel UI behavior verified manually (P1 convention).

## Out of Scope (YAGNI)

- Editing or deleting schedules from the Panel — clicking a row may
  insert the corresponding CLI command, preserving the insert-only
  security model.
- Preview tab (P3) and recommendations (P4) — untouched.
- Schedule firing history / missed-firing records (the underlying
  systems do not persist these).
