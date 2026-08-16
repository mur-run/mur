# Job / fleet run status and lifecycle — design

Date: 2026-08-17
Status: approved (design), not yet implemented

MUR can start work it cannot observe. `parallel_jobs`, `fleet run`, and
`workflow run` all execute through the same DAG executor, and none of them
writes a state anyone can query.

**Every layer has a timer; no layer has a state.** A timeout therefore means
"I stopped waiting" — never "it died" — and there is no second place to go and
ask. The MCP timeout message says so in as many words: *"MUR stopped waiting,
but the server may still be running it — treat the outcome as unknown."* That
message is honest. This design gives it somewhere to point.

Reported symptom: three delegations to `pm` were each reported as an
unknown-outcome MCP timeout at 120 s, then died at ~310 s. Diagnosis required a
manual fold of `~/.mur/channels/<id>/events.jsonl`. The orchestrating agent —
which had the same question — had no way to ask at all. Worse, the `pm` process
was still alive while its work was already dead, so process liveness alone would
have reported `running`.

---

## 0. What exists today (verified against `main` @ 78736ae7)

**Four independent timers, none aware of the others:**

| Value | Location | Effect on expiry |
|---|---|---|
| 120 s | `mur-agent-runtime/src/tools/mcp.rs:17` (`DEFAULT_MCP_TOOL_TIMEOUT_SECS`) | Stops waiting; outcome explicitly unknown |
| 300 s | `mur-core/src/hitl/gate.rs:44` (`DEFAULT_TIMEOUT`), `mur-agent-runtime/src/task_runner.rs:320` | Expiry **= denial** → step FAILS |
| 600 s | `mur-core/src/a2a_dial.rs:43` (`DEFAULT_DIAL_IO_TIMEOUT`) | Socket idle disconnect |
| 30 s | `mur-core/src/parallel/backend/zfs_socket.rs:11` (`AGENT_IO_TIMEOUT`) | zfs agent IO |

**Eight fleet stop reasons** — `LoopStop`, `mur-core/src/cmd/fleet/loop_run.rs:41-61`:
`Converged`, `QueueDrained` (completion); `MaxIterations`, `Deadline`, `Stuck`,
`Budget` (guards); `Stopped`, `CommanderKilled` (external).

**Runtime restart limits** — `mur-common/src/agent.rs:973-988`: `max_restarts: 3`,
`restart_window_secs: 600`, `stop_timeout_secs: 15`.

**The one state file that does exist** — `LockFile`
(`mur-common/src/agent.rs:1144`) classified by `lock_file::classify()`
(`mur-common/src/lock_file.rs:124-146`) into `Running` / `Stale` / `Stopped` via
`pid_alive(lock.pid)`. It is **per agent process, not per run.** Fleet has only
`.stopped` (`mur-core/src/cmd/fleet/control.rs:14`), which is a kill switch, not
a state.

**Nothing on disk can answer "what is this job doing right now."**

---

## 1. The execution unit

One **run** = one `execute_dag` call. All three entry points already funnel
through it:

- `parallel_jobs` — `mur-core/src/executor/jobs.rs:17`
- `fleet run` — `mur-core/src/cmd/fleet/run.rs:378-392`
- `workflow run` — same executor

`run_id` already exists (`mur-core/src/executor/dag.rs:103`) and already seeds
the idempotency keys (`dag.rs:163-165`). It defaults to the empty string
(`dag.rs:130`); **all three entry points must supply it.** A run with an empty
`run_id` is a bug, not a default.

Naming: the CLI surface says `job` (the user-facing word); the internal
identifier stays `run_id`. Do not rename the existing identifier.

---

## 2. State file — a rebuildable cache

`~/.mur/runs/<run_id>/run.json`:

```
schema, run_id, channel_id, kind (job|fleet|workflow), label,
pid, ppid, started_at,
last_heartbeat_at,      // the ONLY field that cannot be rebuilt
state,                  // running | blocked | done | failed | stopped
steps: [{ id, member, state, started_at, ended_at }],
blocked_on: Option<{ hitl_id, summary, since }>,
binary_version, build_sha
```

**This file is a cache, not a source of truth.** When it is missing or suspect,
rebuild it by folding `~/.mur/channels/<channel_id>/events.jsonl` — the
`Delegation`, `StateChange`, `ToolCall`, `ToolResult`, and `HitlRequest` events
are already written there (`mur-common/src/channel.rs:123-133`).

This follows the pattern the codebase already states for itself:
`mur-common/src/channel.rs:106` describes `Channel` as *"The durable manifest (a
cache of state derivable from the event log)"*, and the LanceDB index is
documented as always rebuildable. **Do not create a second source of truth** —
when the cache and the channel disagree, the channel wins and the cache is
rebuilt.

`last_heartbeat_at` is the single exception: it is deliberately not a channel
event, because a 10-second tick would flood an append-only signed audit log and
make manual reading (the thing that diagnosed this bug) impossible. A rebuilt
run therefore carries `heartbeat: unknown` and falls back to pid liveness.
**Reporting `unknown` is required; synthesizing a heartbeat is forbidden.**

---

## 3. Heartbeat and classification — two axes, never flattened

A background task inside `execute_dag` updates `last_heartbeat_at` every
`runs.heartbeat_interval_secs` (config, default 10; no literal in code paths).

`mur_core::run_status::classify()` mirrors `lock_file::classify()` but returns
**two independent axes**:

| Axis | Source | Values |
|---|---|---|
| `state` (semantic) | **stored** — written by the executor | `running`, `blocked`, `done`, `failed`, `stopped` |
| `liveness` | **derived** — never stored | `alive` (heartbeat fresh), `stalled` (pid alive, heartbeat expired), `dead` (pid gone) |

`classify()` reads the stored `state` and computes `liveness` at call time. Only
`state` is persisted; persisting `liveness` would reintroduce the lying-cache
failure this design exists to remove.

Freshness threshold: `runs.heartbeat_stale_after_intervals` (config, default 3).

`liveness` is only meaningful while `state` is non-terminal. For `done`,
`failed`, and `stopped`, `classify()` returns `liveness: n/a` — a finished run's
absent process is not a fault.

**A `kill -9`'d orchestrator writes no terminal state**, so its `run.json` stays
`state: running` forever with `liveness: dead`. That pair — and only that pair —
is what "crashed" looks like. It is *not* terminal: it must keep appearing in
`mur job list` (§4) rather than being filtered out as finished, because a crashed
run is exactly what an operator needs to see.

Flattening these into one enum forces a false question — *"is it blocked or
stalled?"* — when the interesting combinations are cross products:

- `blocked` + `alive` — healthy: waiting for a human, orchestrator fine
- `blocked` + `dead` — the orchestrator died while waiting; the approval will
  never be consumed
- `running` + `stalled` — **the reported bug**: process up, work not moving

`stalled` is the core deliverable of this design. It is the only state that
honestly describes "the process is alive but the work is not moving", which is
exactly what took a long manual investigation to establish.

---

## 4. One derivation, many renderers

`mur_core::run_status::classify()` is the **only** place run state is derived.
Every surface renders its output:

- **CLI** — `mur job list [--all]`, `mur job status <run_id>`,
  `mur job stop <run_id>`, `mur fleet status <name>`
- **MCP** (for agents) — `mur_job_status(run_id)`
- **Hub Panel** — `panel_runs()` in `mur-hub-gui/src-tauri/src/panel/data.rs`

`mur job list` shows every run that is not cleanly finished — including
`running` + `dead` (crashed) — and hides `done` / `failed` / `stopped` unless
`--all` is given. A crashed run must never be silently filtered out.

`mur job stop <run_id>` writes `state: stopped` and signals the orchestrator
process; it is the per-run analogue of `mur fleet stop`, which continues to
operate on the fleet's `.stopped` sentinel
(`mur-core/src/cmd/fleet/control.rs:14`) and is unchanged. Stopping a run does
not clear a fleet's kill switch, and stopping a fleet does not require finding
its run id.

**Any surface that derives run state on its own is a design violation.** This is
not stylistic: the Hub has already shipped this exact bug once (status judged
correctly over the wrong domain). Panel is already built as a thin mirror of
`mur_core` — every function in `panel/data.rs` is a 3–5 line delegation
(`panel_schedule_status` → `mur_core::schedule_status`, `panel_cost` →
`mur_core::cmd::agent::stats`, `panel_proposals` → `mur_core::harvest::proposal`,
`panel_recommend` → `mur_core::recommend`) — so `panel_runs()` must be the same
shape and nothing more.

`Activities` (`panel/data.rs:40-64`) already carries `channels` and `hitl`; runs
join it there.

**`parallel_jobs` becomes non-blocking.** It returns `{ run_id }` immediately
and the caller polls `mur_job_status`. This removes the 120 s wait from the
dispatch path entirely — the fix is to delete the waiting, not to raise the
timeout.

---

## 5. HITL: block instead of deny

`gate()` currently falls back to `DEFAULT_TIMEOUT` when passed `timeout: None`
(`hitl/gate.rs:79`), and expiry denies. Change: `None` means **no timeout**.
The gate writes `blocked_on` into `run.json`, files a notification (§7), and
keeps polling. A run blocked on approval waits indefinitely and stays `alive`.

**The interactive 300 s countdown stays.** In `murmur` a human is watching, and
a countdown is meaningful UI there. Only the unattended delegation path loses
its deadline.

> ⚠️ **Seam with the murmur TUI spec.** The TUI reads `hitl::gate::DEFAULT_TIMEOUT`
> directly to draw its countdown — `mur-core/src/cmd/agent/cli/ui.rs:1118-1123`
> and `mur-core/src/cmd/agent/cli/mod.rs:783`, `:1416`, `:2571`. Changing the
> gate's semantics touches those call sites. **This spec must be implemented
> before the TUI redesign spec**, and the constant must remain exported for the
> interactive path rather than being deleted.

---

## 6. DAG scheduling: wave barrier → ready set

`execute_dag` currently executes in waves: a batch of indices is spawned
together under a semaphore and the whole batch is joined before the next
(`mur-core/src/executor/dag.rs:927-947`). A blocked step therefore holds the
barrier and stalls steps that do not depend on it.

Change to ready-set scheduling: when a step enters `blocked`, its handle is
parked and removed from the join set; the scheduler continues dispatching every
step whose `depends_on` is satisfied. Steps depending on the blocked step stay
pending. The run reports `blocked` only when no step is runnable **and** at least
one step is blocked.

The existing semaphore cap and the topological validation (`dag.rs:267-318`) are
unchanged.

---

## 7. Notification — no new channel

Reuse `mur-open-items` (`report()`, `mur-open-items/src/lib.rs:141`). A run
entering `blocked` files an open item, so it surfaces in `mur open` with no new
plumbing.

The Hub already closes the loop: `channel_hitl_respond()`
(`mur-hub-gui/src-tauri/src/hitl.rs:33`) and `pending_views()` (`:76`) exist, so
a blocked run rendered in Panel is approvable in place — no CLI round trip
through `mur channel approve <channel_id> <hitl_id>`.

---

## 8. Timeout and stop policy

| Timer | Change |
|---|---|
| MCP 120 s (`tools/mcp.rs:17`) | **Keep.** It is a wait timeout, not a killer; non-blocking dispatch means it stops being hit |
| HITL 300 s (`hitl/gate.rs:44`) | **Remove on the delegated path** → `blocked` + notify + wait. Keep for interactive |
| a2a dial 600 s (`a2a_dial.rs:43`) | **Keep.** It exists to stop the CLI hanging forever on a dead peer |
| fleet `Deadline` | **Keep**, but surface it as operator-set rather than ambient |
| `MaxIterations`, `Stuck` | **Keep.** These bound *progress*, not time |
| `Budget` | Split by mode — see below |

**Budget must not be uniformly softened.** `CLAUDE.md` records a safety triad
that may not be weakened: unattended auto-run is off unless `MUR_FLEET_AUTORUN=1`,
auto-run requires a positive `loop.budget_usd`, and the kill switch is
fail-closed. Turning "budget reached → stop" into "budget reached → wait for a
human" removes the brake precisely where no human is present.

- **Attended** (interactive session): budget reached → notify → operator chooses
  continue or stop.
- **Unattended** (`MUR_FLEET_AUTORUN=1`): budget reached → **stop, unchanged.**
  Fail-closed behavior is preserved.

---

## 9. Testing

Happy-path coverage is not sufficient here — the defect being fixed is a status
that lied. Each of the following must fail if its logic breaks:

- Table test over every `(state, liveness)` cell of `run_status::classify()`.
- **Negative control:** `kill -9` the orchestrator → must classify `dead`. A test
  that only asserts `running` for a live process proves nothing.
- **Negative control:** freeze the heartbeat while keeping the pid alive → must
  classify `stalled`, not `running`. This is the reported bug; it must be
  reproducible on demand.
- Non-blocking dispatch: `parallel_jobs` returns a `run_id` without waiting, and
  `run.json` appears within a bounded interval.
- Ready-set scheduling: one blocked step must not delay an independent step.
- Rebuild: delete `run.json`, fold from the channel, assert the result reports
  `heartbeat: unknown` rather than a fabricated value.
- Budget split: unattended mode still stops at the ceiling.

---

## Scope

**In:** run state file and rebuild path; heartbeat; `run_status::classify()`;
`mur job` CLI; `mur_job_status` MCP tool; non-blocking `parallel_jobs`; HITL
block-instead-of-deny on the delegated path; ready-set DAG scheduling; open-item
notification; Panel rendering; budget mode split.

**Out:** run retention/GC policy (terminal `run.json` files simply accumulate for
now); the murmur TUI status line (belongs to the TUI redesign spec, which is
coupled to its layout rework); cross-machine run queries.

---

## Deployment note

`mur-hub-gui` is workspace-excluded, so `cargo build --workspace` does **not**
compile Panel. Any change to a `run_status` public type can break
`panel/data.rs` without CI's ordinary path noticing. Changes to `run_status`
public types must additionally build the Hub through its own manifest before the
work is called done.
