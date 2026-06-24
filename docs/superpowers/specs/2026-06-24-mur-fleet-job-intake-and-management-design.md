# MUR Fleet — Job Intake & Roster Management Design

**Date:** 2026-06-24
**Status:** Design (approved for planning)
**Scope:** `mur fleet` CLI + store + daemon `fleet_tick`

## Problem

A fleet today is a goal-driven squad: the `goal` field in `~/.mur/fleets/<name>/fleet.yaml`
is the work, and `run` / `run --loop` / the daemon `fleet_tick` all execute that one stored
goal. Consequences:

1. **Dispatching a task means hand-editing `fleet.yaml`.** To give a fleet a new job you open
   the file and rewrite `goal`. This is the core pain point — a fleet is a durable squad, but
   the *work* you hand it is ephemeral, and the two are conflated in one file.
2. **`mur fleet list` is unreadable.** Each fleet prints one long wrapped line ending in the
   full multi-line goal, so the list wraps into an illegible block.
3. **Roster management is missing.** No `add` / `remove` / `delete` — editing membership also
   means hand-editing YAML (and the channel roles drift out of sync).

## Goals

- Dispatch a job to a fleet **by command**, never by editing a file.
- Keep `goal` as the fleet's *standing mission* (default work) — not the dispatch mechanism.
- A scannable `mur fleet list`.
- First-class `add` / `remove` / `delete`.
- Job model aligned with the **A2A Task lifecycle** so A2A intake is a thin follow-on, not a
  rewrite.

## Non-Goals (this phase)

- **A2A job intake.** A fleet exposing an A2A endpoint that external/other agents dial to hand
  it a job is a documented follow-on (see "A2A follow-on" below). This phase is CLI-only.
- Per-job streaming / push notifications.
- Multi-job parallelism. Jobs are processed **one at a time, oldest first** (FIFO). A fleet's
  internal member fan-out is already parallel; the *queue* is serial.

## Core model

A fleet is a **durable squad**. Work is **ephemeral** and enters as a **job**.

- `goal` (fleet.yaml) = **standing mission / default job**. Used when no job is queued and no
  job arg is given (i.e. daemon auto-run and bare `mur fleet run <name>`). It is no longer the
  way you dispatch ad-hoc work.
- A **job** = a unit of work handed to the fleet by command. It becomes the goal *for one run*.

### Job record

Path: `~/.mur/fleets/<name>/jobs/<id>.yaml`
`id` = UUIDv7 (time-sortable — FIFO ordering is filename sort, no index file).

```yaml
id: 0190f3a2-...            # uuid v7
text: "Refactor the model-registry module for clarity"
source: cli                 # cli | a2a:<agent-id>   (a2a is follow-on)
status: queued              # queued | running | done | failed | canceled
created_at: 2026-06-24T...  # RFC3339
started_at: ~               # set when picked up
finished_at: ~              # set on terminal state
run_id: ~                   # links to the channel run that executed it (results live there)
result: ~                   # short summary on done
error: ~                    # message on failed
```

**Status ↔ A2A `TaskState` mapping** (deliberate, for the follow-on):

| job status | A2A TaskState |
|------------|---------------|
| queued     | submitted     |
| running    | working       |
| done       | completed     |
| failed     | failed        |
| canceled   | canceled      |

`input-required` / `auth-required` are out of scope this phase (no interactive pause); a future
job can grow an `input-required` status without breaking the enum (serde-tolerant).

### Store API (`cmd/fleet/jobs.rs`, new)

```
jobs_dir(home, name) -> PathBuf                      // ~/.mur/fleets/<name>/jobs
enqueue_job(home, name, text, source) -> Job          // write <id>.yaml, status=queued
list_jobs(home, name) -> Vec<Job>                     // sorted by id (FIFO)
next_queued(home, name) -> Option<Job>                // oldest status==queued
update_job(home, name, &Job)                          // atomic temp+rename, like save_fleet
```

Atomic write (temp + rename), same pattern as `store::save_fleet`. Fleet-name validated via
`valid_fleet_name` (defense-in-depth, like the store layer).

### Commands

- **`mur fleet send <name> "<job>"`** — enqueue a job (`status=queued`, `source=cli`), print
  the job id. Asynchronous: it does **not** run. Drained by `mur fleet run` or the daemon.
- **`mur fleet run <name> ["<job>"]`** — synchronous, one iteration. Goal resolution order:
  1. explicit `<job>` arg → run it as a one-shot (also persisted as a job, `done` on finish);
  2. else oldest `queued` job → run it, mark `running` → `done`/`failed`;
  3. else the standing `goal`.
  (Bare `mur fleet run <name>` keeps today's behavior when the queue is empty.)
- **`mur fleet jobs <name>`** — list jobs with status, id (short), age, and result/error.
  `--all` to include terminal jobs; default shows queued + running + recent.

### Queue draining

The queue is drained by the existing executors — no new daemon, no new loop:

- **`mur fleet run`** (above) pops one job per invocation.
- **`mur fleet run --loop`** (`loop_run.rs`): each iteration first checks for a queued job;
  if present it runs that job (marking running→terminal), else falls back to the standing goal
  (today's behavior). All existing guards (iteration cap / deadline / budget-usd / stuck /
  kill-switch / commander governance) apply unchanged.
- **daemon `fleet_tick`** (`mur-daemon/src/fleet_tick.rs`): when a fleet is due, if it has
  queued jobs it drains the oldest (gated by the same `MUR_FLEET_AUTORUN` + positive budget +
  kill-switch triad). Empty queue → today's standing-goal auto-run behavior.

Failure handling: an executor error marks the job `failed` with the error message and stamps
`finished_at`; the loop/daemon continues per its existing guard policy (fail-safe). A job
whose run is killed mid-flight stays `running` and is re-picked only if explicitly retried
(no silent infinite retry — name the ceiling in code).

## `mur fleet list` — aligned table

Replace the wrapped one-liner with a column-aligned table (one row per fleet, goal truncated to
the remaining terminal width):

```
NAME          ST  MEM  JOBS  ROUTER  GOAL
model-reg     ●   3    0     mur     Refactor the model-registry module for clarity
develop-rust  ●   6    2     mur     Implement the plan at /Volumes/…/phase1.md task by …
rust-solo     ⏸   1    0     mur     Implement ONLY Task 3 from the plan at /Volumes/…

● idle   ⏸ stopped   ▶ running
```

- `ST`: `⏸` if kill-switch (`control::is_stopped`); `▶` if a job is `running`; else `●` idle.
- `MEM`: member count (full roster is in `show`).
- `JOBS`: count of `queued` jobs — surfaces who has pending work at a glance.
- `GOAL`: standing goal, truncated with `…` to fit the line. Newlines collapsed to spaces.
- Column widths computed from the rows; goal gets the remainder of `$COLUMNS` (fallback 80).

`mur fleet show <name>` is unchanged — it remains the full-detail view (and can additionally
print the job list).

## add / remove / delete

All three keep `fleet.yaml` and the shared channel `fleet-<name>` in sync (membership lives in
both; today only `create` writes them together).

- **`mur fleet add <name> <agent>...`** — canonicalize each name
  (`a2a_dial::canonicalize_agent_name`), append to `members` if not already present, and add the
  member to the channel with the `Delegate` role. Idempotent (existing member is skipped). Save.
- **`mur fleet remove <name> <agent>...`** — remove from `members` and from the channel.
  Refuse to remove the current `router` (error: "router 'x' cannot be removed; set a new router
  first"). Removing a non-member is a no-op warning, not an error. Save.
- **`mur fleet delete <name> [--yes]`** — delete the fleet directory
  (`~/.mur/fleets/<name>/`, including `jobs/` and sentinels) **and** the shared channel
  `fleet-<name>`. **Member agents are never touched** — they are independent, shared resources.
  Prompts for confirmation unless `--yes`. `delete` does not check for a running loop — the
  kill-switch sentinel is the stop mechanism, and the running loop bails when its files vanish
  at the next guard check. Deleting a fleet with a running loop is the operator's call.

Channel role constants and the add/remove channel mutation reuse `mur_channel::ChannelService`
(the same API `create::create_for_fleet` already uses). If the service lacks an
add-member/remove-member primitive, add the minimal one there.

## A2A follow-on (documented, not built this phase)

When built, A2A intake reuses everything above:

- The fleet's router/concierge accepts an A2A `message/send` whose message text is the job.
- The handler calls `enqueue_job(..., source = "a2a:<caller-id>")` and returns an A2A **Task**
  whose `id` is the job id and whose state mirrors the job status (`submitted`/`working`/…).
- `tasks/get` maps to reading the job record; the result artifact is the job's `result` +
  the channel run referenced by `run_id`.
- Blocking (`return_immediately:false`) vs async maps to drain-now vs enqueue.

No storage or model change is needed — the job IS the A2A Task. That is the point of aligning
the status enum now.

## Security & safety

- **No blanket approval.** `send`/`run`/drain pass `yes:false` to the DAG executor exactly as
  today (fail-closed; risk-tiered steps still gate). A job is just a goal string — it grants no
  new authority.
- **Name validation** on every job-store path (`valid_fleet_name`) — a job file can never be
  written outside `~/.mur/fleets/<name>/jobs/`.
- **`delete` is destructive and confirmed** (`--yes` to skip). It removes the channel (and its
  audit history) — call this out in the confirmation prompt.
- **Auto-run triad unchanged.** Daemon queue-drain is still gated by `MUR_FLEET_AUTORUN` +
  positive `budget_usd` + kill-switch. The queue does not create a new unattended path.
- **Job text is untrusted input** once A2A intake lands; this phase is CLI-only (operator is the
  source) but the job text is treated as a plain goal string, never shell/path-interpolated.

## Testing

Unit (`cargo nextest`, `ORT_STRATEGY=download`):

- jobs store: enqueue → list (FIFO by uuid v7) → next_queued → update → terminal; atomic write;
  invalid fleet name refused.
- `run` goal-resolution order: arg > queued > standing goal; empty-queue back-compat.
- `list` rendering: truncation, status symbol selection, job count, newline collapse.
- `add`: idempotent, canonicalizes, syncs channel role.
- `remove`: refuses router, no-op on non-member, syncs channel.
- `delete`: removes dir + channel, leaves member agents, honors `--yes`/confirmation.

CLI integration (`tests/cli_fleet.rs`): `create → add → send → run (drains job) → jobs (done)
→ remove → delete` round-trip on a temp home.

Live (operator): `send` then `run --loop` drains across iterations; daemon drain with autorun
triad set.

## Files touched

- `mur-common/src/fleet.rs` — `Job` struct + status enum (new; or a `fleet_job.rs` sibling).
- `mur-core/src/cmd/fleet/jobs.rs` — new (store API + `send`/`jobs` commands).
- `mur-core/src/cmd/fleet/run.rs` — goal resolution (arg > queued > goal); persist one-shot.
- `mur-core/src/cmd/fleet/loop_run.rs` — per-iteration queue check.
- `mur-core/src/cmd/fleet/list.rs` — table renderer.
- `mur-core/src/cmd/fleet/roster.rs` — new (`add`/`remove`); or extend `control.rs`.
- `mur-core/src/cmd/fleet/delete.rs` — new (or in `control.rs`).
- `mur-core/src/cli/actions.rs` — `FleetAction::{Send, Jobs, Add, Remove, Delete}` + `run` job arg.
- `mur-core/src/dispatch.rs` — wire the new actions.
- `mur-daemon/src/fleet_tick.rs` — queue-drain branch.
- `mur-core/src/cmd/fleet/store.rs` — `jobs_dir`; channel add/remove member helper if needed.

Keep each file ≤ 800 lines (Mandatory Rule 4) — `import.rs`/`loop_run.rs` are already large;
new surface goes in new sibling modules, not bolted onto them.
