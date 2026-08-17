# Fleet run status: one record per run, one truth for three surfaces

**Date:** 2026-08-17
**Status:** design, not implemented
**Spans:** `mur-common` (progress schema v2), `mur-core` (run records, status
model, reconcile v2, CLI `status`/`runs`/`logs`), `mur-hub-gui` (status
command), murmur TUI (rail data source)

## Problem

A fleet's work has three interested surfaces — CLI, murmur TUI, Hub — and no
shared answer to the four questions a user actually asks: *is it running, what
is it doing, what did it cost, what happened last time?* Each surface derives
its own partial answer from a different store, and several of those answers
are provably wrong. This is the same disease the murmur UX batch diagnosed
(A1–A7): one fact, told differently by every interface that touches it.

Concrete defects, each verified in code:

### 1. A crashed run's job lies for six hours, then lies differently

`reconcile_running` (`jobs.rs:88-125`) has a channel-truth arm that adopts the
channel's terminal state for a zombie `running` job — but it is gated on
`job.run_id.is_some()`, and **no production write path ever produces a
`running` job with a `run_id`**. Both writers stamp `run_id` in the same save
as the terminal status (`run.rs:405-407`, `loop_run.rs:591-593`);
`resolve_run_goal`/`iteration_goal` save the `Running` transition without it
(`run.rs:181-185`, `loop_run.rs:235-239`). The arm is unreachable. The unit
test that covers it fabricates the state by hand
(`mur-core/src/cmd/agent/cli/fleet_rail/tests.rs`, lines 433-436) — it
passes while proving nothing about the production flow.

Consequence: every crashed run takes the fallback path — six hours of
`RUNNING_GRACE_SECS` (`jobs.rs:16`) showing `running`, then
`failed (orphaned)` — **even when the channel recorded `completed`** before
the crash.

### 2. A live run longer than six hours is falsely failed mid-flight

The orphan rule has no liveness signal, only wall clock. A legitimate 7-hour
loop (deadline `12h`) produces channel activity but no terminal `StateChange`
until the end, so at hour six the reconciler flips its job to
`failed (orphaned)` while the run is still working; the end-of-run stamp later
overwrites it back. Status flip-flops through states that were never true.

### 3. A standing-goal run is invisible

`mur fleet list` derives ▶ running from job records (`list.rs:63-71`), and the
Hub does the same. Goal-mode runs never touch the job store, so a fleet
mid-run shows ● idle everywhere except the murmur TUI — which patched this
locally with `run_in_flight`, knowledge only the invoking TUI has
(`fleet_rail.rs:207-238`). A run started by the daemon or another terminal is
invisible to everyone.

### 4. The loop path still tells the #10 lie, and can leave jobs running

`run.rs` learned (#10) that a DAG can return `Ok` while the channel ended
`failed`, so it reads the channel back before stamping the job
(`run.rs:394-448`). The loop never got the fix: it stamps `Done`
unconditionally (`loop_run.rs:590-597`), and on an execution error the `?` at
`loop_run.rs:586-588` propagates before any stamp — the claimed job stays
`Running` and waits for defect 1's six-hour path.

### 5. Three run identities, zero joins

A single run mints `run-<uuidv7>` (`run.rs:377`); each loop iteration mints
`loop-<name>-<uuidv7>-<iter>` (`loop_run.rs:582`); the progress file mints a
third, bare UUID (`loop_run.rs:390`). None of them can be recovered from the
channel: `run_id` goes into idempotency keys only as a SHA-256 input
(`idem_key`, `mur-core/src/executor/dag.rs`, lines 163-165). Given a job's
`run_id` there is nowhere to look
it up; given a channel there is no way to slice it into runs except a seq
cursor someone remembered at the time.

### 6. There is no run history, and nothing reads what little exists

`.run_progress.json` is loop-only, overwritten by the next run
(`progress.rs:9-11`); single runs write nothing (no progress, no `on_step`
observer). No CLI command reads it — its only consumer is the deep-research
panel. The loop's stop reason (`converged` / `budget` / `stuck` …) reaches
exactly one place: the invoker's stdout. For a daemon-triggered loop, that is
a log line in `tracing` and nothing else.

### 7. The Hub's "last run" is a scheduler cursor, not a fact

The Hub renders `.last_run` (`hub fleet.rs:81-88`) — a file written only by
the daemon's `fleet_tick` as its due-check cursor. Manual, Hub-invoked, and
TUI-invoked runs never update it, so "last run" in the Hub is wrong for every
run the user started themselves.

## What already exists

The raw material is mostly there; it is unjoined and single-consumer:

- **Job store + lazy reconcile** — `jobs/<uuidv7>.yaml`, status lifecycle
  aligned to A2A TaskState, reconciled on every list (#494, #10).
- **`RunProgress`** (`progress.rs`) — run_id, goal, iteration, budget/spend,
  per-step state/worker/cost, outcome; atomic best-effort save; an mtime
  staleness concept (`STALE_AFTER_SECS`). 90% of the run-record schema.
- **The channel** — the complete, signed event history: `Delegation`
  (turn start, `target_agent` + `goal`), member `Message` (signed reply),
  `StateChange`, `ToolCall`, `HitlRequest/Response`. `seq` is total order.
- **`fold_members`** (`fleet_rail.rs:79`) — channel events → per-member
  state, already shaped for the production delegate path (#878).
- **`follow::milestone` / `follow::summarize`** (`follow.rs:154,236`) —
  channel events → human lines, already tested.
- **`.stopped`** kill-switch; **`.last_run`** daemon cursor; the skill
  event-log `record_run` ledger (stats pipeline — a different consumer, left
  alone).
- Prior specs: job intake (2026-06-24), murmur rail (2026-07-29), loop
  settings (2026-08-02). This spec is the missing fourth leg: the status
  substrate they all assumed.

## Design

Principle: **a run is the unit of status, and its record is written by the
run itself.** One identity, one durable record with a heartbeat, one truth
function, one snapshot type that every surface renders. Everything below is
an extension of an existing primitive — no new event system, no daemon
dependency, no channel schema change.

### §1 One run identity, stamped at claim time

- Mint `run_id = run-<uuidv7>` **before goal resolution**, on both paths.
- `resolve_run_goal` / `iteration_goal` take the `run_id` and stamp
  `job.run_id` in the same save that marks the job `Running`. This single
  line makes defect 1's reconcile arm reachable and gives a live job a
  navigable link from the moment it is claimed.
- Loop iterations derive their DAG nonce from the run:
  `<run_id>:<iteration>`. Idem-key uniqueness across concurrent loops and
  determinism within an iteration (crash-resume, v3c cursor) are preserved;
  the *run* identity stops fragmenting per iteration.

### §2 A record per run: `~/.mur/fleets/<name>/runs/<run_id>.json`

`RunProgress` becomes schema v2 and per-run. Additive fields, one rename:

```yaml
schema_version: 2
run_id: run-0198f2…       # minted at start (§1)
fleet: dev
mode: single | loop
invoker: cli | daemon | hub | tui    # best-effort provenance
pid: 48231
goal: "…"                 # RESOLVED goal (job text or standing); was
                          # `question` — read alias kept, so v1 files load
job_ids: [ "0198…" ]      # jobs claimed by this run (a loop may claim many)
start_seq: 42             # channel seq before the run's first event
end_seq: 97               # stamped at finish; ABSENT after a crash
started_at / finished_at / outcome / iteration / model
budget_usd / spend_usd / steps: [ … ]   # unchanged from v1
```

- **Both paths write it.** `run.rs` gains the record + the same `on_step`
  observer the loop already has (factored out, not duplicated), so single
  runs get live per-step state for free.
- **`start_seq`/`end_seq` are the channel join.** `run_id` is not recoverable
  from events (defect 5) and we do not change the channel schema; the seq
  window is how `fleet logs --run` (§5) and the reconciler scope a run. A
  crashed run has no `end_seq`; consumers bound its window by the next
  record's `start_seq`, else the channel end.
- **Heartbeat = file mtime.** Step events already save the record; in
  addition the run spawns a trivial ticker (tokio interval, aborted at exit)
  that touches the record every `RUN_HEARTBEAT_SECS = 60`, so a single
  long-running delegate step cannot read as dead. Liveness rule:
  `finished_at == None && mtime age < RUN_STALE_SECS (300)` → live; older →
  the run's process is gone.
- **`.run_progress.json` stays** as a dual-written copy of the newest run
  (compat: deep-research panel reads it; in-repo consumers move to the run
  directory at leisure).
- **Retention:** prune the run directory to the newest `RUNS_KEPT = 50` at run start
  (`run-<uuidv7>` filename sort is time sort, same trick as the job queue).
- **Concurrent runs** of one fleet are legal today (the loop's idem nonce
  exists exactly for that, `loop_run.rs:580-582`); the model simply allows
  N live records and surfaces list them all, newest first.

### §3 One truth function for terminal state and liveness

A new `status.rs` module under `cmd/fleet/` owns the rules; `run.rs`,
`loop_run.rs`, and the reconciler all call it instead of reimplementing:

- **`stamp_job_terminal(job, run, exec_result, events)`** — the #10 logic
  from `run.rs`, factored and window-scoped (`seq ∈ (start_seq, end_seq]`),
  used by **both** paths. The loop wraps `execute_dag` so an `Err` stamps the
  claimed job `Failed` and the record `outcome: failed` *before* propagating
  (closes defect 4 on both edges).
- **Reconcile v2** (replaces the dead arm in `jobs.rs`): for a `Running` job,
  resolve its run record by `run_id` (present since §1):
  - record **live** (heartbeat fresh) → leave the job alone. This deletes
    defect 2: no wall-clock guess can fail a run that is demonstrably alive.
  - record **dead or finished** → adopt the channel's terminal `StateChange`
    *within the run's window*; if none, `failed ("run died mid-flight")`.
    Window scoping means another run's terminal event can never be
    misattributed — the hazard the old `run_id.is_some()` gate was guarding
    against, solved instead of suppressed.
  - **no record** (legacy job, pre-migration) → the existing 6h wall-clock
    rule, unchanged, as the fallback of last resort.
- **Outcome vocabulary:** single runs use the channel's —
  `completed | failed | canceled`; loops keep the `LoopStop` labels
  (`converged | max-iterations | deadline | budget | stopped | stuck |
  commander-killed | queue-drained`) plus `failed` for the error path above.

### §4 One snapshot type: **FleetStatus**

```rust
pub struct FleetStatus {
    pub name: String,
    pub stopped: bool,                 // kill-switch
    pub live_runs: Vec<RunSummary>,    // heartbeat-fresh records, newest first
    pub last_run: Option<RunSummary>,  // newest finished record
    pub queued: usize,
    pub next_queued: Option<JobBrief>, // id + text head
    pub members: Vec<MemberRow>,       // fold_members over the newest live
}                                      // run's window; empty when idle

pub struct RunSummary {                // projected from RunProgress
    pub run_id: String, pub mode: RunMode, pub goal: String,
    pub job_ids: Vec<String>, pub started_at: String,
    pub finished_at: Option<String>, pub outcome: Option<String>,
    pub iteration: u32, pub spend_usd: f64, pub budget_usd: Option<f64>,
    pub totals: Totals,                // steps done/running/pending/failed
    pub heartbeat_age_secs: Option<u64>,
}
```

Computed in one place (`status.rs`), `Serialize` end to end. The CLI renders
it as text, the Hub returns it as JSON, the TUI reads the pieces it needs.
No surface derives status from raw stores anymore — that is the A1–A7 lesson
made structural.

### §5 CLI surface

**mur fleet status** `<name> [--json] [--watch]` — the one-stop answer:

```
Fleet: dev
Run:   run-0198f2… loop · iteration 3 · LIVE (heartbeat 12s ago)
  goal:  Fix issue #942 — TUI gate focus discipline
  spend: $0.84 / $5.00 · model deepseek_v4
  steps: 2✓ 1⏵ 1 pending
  ▲ qa   approval needed  (mur channel approve fleet-dev h-3f2a)
  ⏵ pm   cargo nextest    4m
Queue: 2 queued · next 0198aa… "Refactor the settlement card…"
Last:  run-0198e1… converged · 4 iterations · $1.20 · 2h ago
```

Idle fleets print the queue + last-run lines only; a stopped fleet leads
with the kill-switch line `show` already prints. `--watch` is a plain
2-second re-render loop (the rail's poll cadence), nothing fancier.
`--json` serializes **FleetStatus** verbatim.

**mur fleet runs** `<name> [<run-prefix>]` — history from the run directory:

```
RUN       WHEN    MODE    OUTCOME     ITER  SPEND  GOAL
0198f2…   2m ago  loop    (live)      3     $0.84  Fix issue #942 — TUI…
0198e1…   2h ago  loop    converged   4     $1.20  nightly triage
0198aa…   1d ago  single  completed   1     $0.09  Update README for…
```

With a prefix: the full record — steps table, job links, seq window, and the
exact `fleet logs dev --run 0198f2` invocation to go deeper. Prefix resolution
copies the job-cancel contract (unique prefix or refuse).

**mur fleet logs** `<name> [--run <prefix|last>] [--all] [--follow]` — the
channel, rendered through the existing `follow::milestone` formatter (moved
to a TUI-independent home; the TUI keeps calling it):

```
14:02:11 → qa  Fix issue #942 — TUI gate focus…
14:06:40 ← qa  replied (4m29s): PR #951 opened, tests green
14:06:41 ⚑ completed
```

Default scope is the newest run's seq window; `--all` is the whole channel;
`--follow` polls like the TUI's milestone follow. This is the "member
replies are invoker-only" gap (#878 leftover), closed.

**Touch-ups to existing commands** (no new flags):

- `mur fleet list` — ST ▶ now means "a live run record exists" (standing-goal
  runs finally visible; defect 3). Jobs-derived running remains as input, not
  the definition.
- `mur fleet show` — one appended line, `Status: …` from **FleetStatus**
  (live run or last outcome + when), pointing at **mur fleet status**.
- `mur fleet jobs` — two columns added: age/duration and `RUN` (short
  `run_id`), so a job row leads somewhere.

### §6 Hub, TUI, daemon adoption

- **Hub:** one new command `fleet_status(name)` returning a serialized
  **FleetStatus**; `fleet_list` gets `live` and `last_run` from run records.
  `.last_run` goes back to being what it is — the daemon's scheduling
  cursor — and stops being rendered as a fact (defect 7). Existing
  `fleet:run_done` events unchanged.
- **murmur TUI:** the rail's `jobs_line` truth upgrades to **FleetStatus**, so
  "⏵ run in progress" appears for runs started *anywhere*, not only ones this
  TUI launched. Auto-arming the rail stays `StepStarted`-triggered — a rail
  materializing because the daemon woke up elsewhere would be motion the user
  didn't cause; the collapsed line carrying the fact is enough.
- **Daemon:** no code change beyond what `loop_run` gives it for free. Its
  runs now leave durable records with outcomes, so "why did the nightly loop
  stop" finally has an answer outside `tracing`.

### §7 Migration and compatibility

- All `RunProgress` consumers are in-repo; v2 renames `question` → `goal`
  with a serde read-alias, everything else is additive with defaults, so v1
  `.run_progress.json` files still load and old readers were updated in the
  same release. `schema_version: 2`.
- Jobs: only *when* `run_id` is written changes; the field's meaning does
  not. Terminal-stamped legacy jobs stay valid; legacy `Running` zombies take
  the unchanged 6h fallback.
- **No channel schema change** (`CHANNEL_SCHEMA_VERSION` untouched). Run
  scoping is seq-window math on the run record, deliberately — embedding
  `run_id` in event payloads would be a second source of truth for the same
  join and a migration for every signed-event reader.
- File-size rule: the new logic lands in `status.rs` + `runs.rs`; `run.rs`
  (701 lines) and `loop_run.rs` shrink or hold via the §3 extraction, they do
  not grow.

## Testing

The reconcile suite currently proves the wrong thing (defect 1); the fix is
as much about the tests as the code:

- **Construct states the way production writes them.** The
  fabricated-`run_id` rail test is replaced by: claim a job through
  `resolve_run_goal` (asserting `run_id` is present while `Running` — the
  regression gate for §1), crash-simulate by writing the channel terminal
  without the job stamp, and assert reconcile adopts `Done` — the exact #10
  crash, finally reachable end to end.
- Liveness matrix: fresh heartbeat → untouched; stale + terminal-in-window →
  adopted; stale + nothing → `failed (run died mid-flight)`; no record →
  legacy 6h path.
- Window scoping: a previous run's `completed` must not terminate the next
  run's job (two records, disjoint seq windows).
- Loop error path: `execute_dag` `Err` seam → job `Failed`, record
  `outcome: failed`, error still propagates.
- Heartbeat ticker: mtime refreshes across a step-event-free interval.
- CLI: `status` / `runs` / `logs` against a seeded tempdir home (house
  pattern from `jobs.rs`/`loop_run.rs` tests); `--json` round-trips
  **FleetStatus**.

## Files touched

- `mur-core/src/cmd/fleet/progress.rs` — schema v2, per-run paths, prune,
  liveness helpers
- `cmd/fleet/` — new `status.rs`: **FleetStatus**, truth rules,
  `stamp_job_terminal`, reconcile v2 core
- `cmd/fleet/` — new `runs.rs`: `mur fleet runs` + record listing
- `mur-core/src/cmd/fleet/{run,loop_run}.rs` — mint-at-start, claim-time
  stamp, shared observer, error-path stamp, record writes
- `mur-core/src/cmd/fleet/{jobs,list,show}.rs` — reconcile v2 wiring, ST from
  live records, status line, jobs columns
- `cmd/fleet/` — new `logs.rs`, plus the `follow.rs` formatter extraction
- `mur-core/src/cli/actions.rs` — `Status`/`Runs`/`Logs` variants
- `mur-hub-gui/src-tauri/src/fleet.rs` — `fleet_status`, `fleet_list` fields
- `mur-core/src/cmd/agent/cli/fleet_rail.rs` — `jobs_line` source swap
- `docs` — README + docs site + product page via the `update-docs` skill

## Deliberately not in v1

- **Per-tool-call member events in the fleet channel** — that is A2
  peer-writes (v3d-2 follow-on); the rail and `logs` render what exists.
- **Hub timeline UI** — the Hub gets the data (`fleet_status`); rendering a
  member timeline is a Hub design of its own.
- **Notifications on outcome** (companion nudge, push) — separate concern;
  the durable outcome makes it possible later.
- **Canceling a `running` job** — unchanged position (`jobs.rs:219-220`):
  `mur fleet stop` is the kill-switch; a job-level kill would need process
  ownership the store doesn't have.
- **Config knobs for retention/heartbeat/staleness** — named constants until
  someone real needs to tune them.
- **A2A status endpoint** — the A2A job-intake follow-on can serve
  **FleetStatus**; nothing here blocks it.
- **Unifying with the skill event-log ledger** (`record_run`) — that ledger
  feeds skill stats, a different consumer with different retention; joining
  them would couple the learning pipeline to UI status.
