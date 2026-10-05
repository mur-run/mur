# Agent review loop — Phase 1 design

- **Status:** Approved rev 3. **§2.3 A1–A4 and P1–P4 approved.** The human read the final §8.2
  (including the corrupted-marker addition and AC11g) on 2026-10-04. **AC0 is satisfied;**
  implementation may start per §15.
- **Rev 2 (2026-10-04):** §14 Q1–Q7 answered by the human and folded into the body. A4 scope made
  explicit (ordinary fleets unchanged). Replay-failure degradation added (§8.2, AC11a–AC11c).
- **Rev 3 (2026-10-04):** P1–P4 decided (§14.2). §8.2 gains monotonic clock/cost on rollback and
  the restart of round N+1 with a fixed restart note. AC11d–AC11f added.
- **Date:** 2026-10-04
- **Source:** design summary approved in brainstorm (Mode A, architecture decision A).
- **Owner (spec):** PM. **Build:** coding agent. **Verify:** QA. **Ship:** GitHub Manager.

---

## 0. Scope

Phase 1 delivers **Mode A — the reviewer loop**: a *main* agent produces work, a *reviewer* agent
reviews it, and they alternate until the reviewer approves, the loop blocks or escalates, or a limit
trips. Agents talk over the existing **A2A** transport. A human attends the loop in **MURMUR**
(`mur agent cli`) in semi-auto or auto mode.

In scope: the loop core (state machine, verdict, finding ledger, rebuttal/escalation, stop
conditions), human intervention in MURMUR, semi-auto and attended auto mode, and error handling.

Out of scope: see §13.

## 1. Problem and why now

**Problem.** When an agent's output needs a second opinion today (a spec checked by QA, a patch
checked by a reviewer), the human relays the work. They copy output from one agent to the other,
remember which objections were raised, decide whether a reply actually addressed them, and notice
when the two agents are arguing in circles. The relay is manual and nothing is recorded. Objections
get lost between rounds, a "fixed" claim is never checked against the original finding, and there
is no point at which the loop is known to be stuck or too expensive.

**Why now.** The pieces already exist separately: fleets over a signed channel
(`docs/architecture/fleet.md`), the execution limits `deadline / stuck / cost_usd`
(`mur-common/src/limits.rs`), the `mur fleet stop` kill-switch (`mur-core/src/cmd/fleet/control.rs`),
and a HITL gate. What is missing is a structured review protocol on top of them: a verdict, a
finding ledger, and escalation. Without that protocol, the loop cannot be run safely except by hand.

> Assumption (not verified): no usage data exists on how often users relay between agents by hand.
> The problem statement comes from the brainstorm, not from metrics.

## 2. Precondition — fleet + channel fitness (MUST be approved before this spec)

The approved design requires: "confirm fleet + channel support a run that is ephemeral, 2-member,
attended, and pausable/resumable. If not, the spec must propose an adjustment … Do not silently
fall back to a client-side orchestrator."

### 2.1 Findings (verified against `main` @ `0e5b8fde`)

> Rev 1 cited `0c87836c`, which is on `docs/cookbook-mur-home` and is not an ancestor of `main`.
> Between it and `main` @ `0e5b8fde` the only changes in the cited files are in `guarded.rs`
> (`StepEventKind::Skipped` handling) and `status.rs`. Every citation below was re-checked
> at `0e5b8fde`, and the cited lines are unchanged.

| Requirement | Supported today? | Evidence |
|---|---|---|
| **2-member** | **Yes** | `Fleet.members: Vec<String>` (`mur-common/src/fleet.rs:31`). A hand-written `procedure:` with `depends_on` is honoured without the router (`plan.rs::static_procedure`). |
| **Attended** (no `MUR_FLEET_AUTORUN`) | **Yes** | The autorun gate applies only to the daemon `fleet_tick` (`fleet.md` §2: "Gated: `MUR_FLEET_AUTORUN=1` and a positive `loop.budget_usd`"). A manual `mur fleet run` needs no env flag. |
| **`limits` resolve** | **Yes** | `loop_run::fleet_bounds` resolves flag > agent > fleet > global > built-in (`mur-common/src/limits.rs::resolve`). |
| **`mur fleet stop` kills it** | **Yes, cooperatively** | `.stopped` sentinel; "a running loop bails at the next iteration" (`control.rs` header). Checked once per iteration (`guarded.rs:309`). |
| **Ephemeral** | **Partial** | No ephemeral-fleet concept: `fleet create` persists `~/.mur/fleets/<name>/fleet.yaml`. `fleet delete` exists, but it **removes the channel and its audit history** ("Destructive: also removes the channel's audit history", `delete.rs`). That breaks the requirement that the ledger stays available for replay and audit. |
| **Pausable / resumable** | **No** | Nothing in `cmd/fleet/`, `fleet.rs`, `mur-channel`, or `fleet_tick.rs` provides pause/resume of a fleet run. The nearest primitives are (a) `LoopStop::AwaitingApproval`, which **ends** the loop ("approve … then re-run this fleet", `run.rs:587`), (b) the `.stopped` kill-switch, which is a stop, and (c) commander governance `kill`/`resume` directives (`mur-channel/src/governance.rs`), which gate a fleet but do not checkpoint a run. `fleet-state/<name>/progress` is "overwritten by the next run" (`progress.rs`), so it is not resumable state. |
| **Deadline excludes paused time** | **No (but reusable)** | `run_guarded` measures `start.elapsed()` from an `Instant` (`guarded.rs:133,314`), which is wall-clock. The pure guard `check_guards(iteration, elapsed, deadline, stuck_for, stuck)` takes `elapsed` as a parameter, so it can be fed execution time instead. |
| **Turn protocol fits the existing loop** | **No** | The existing loop iteration is "router plans a DAG → members run → convergence via `done_when` marker / queue-empty / router DONE" (`loop_run/mod.rs`, `done_policy.rs`). It has no verdict, no per-finding state, and no human input between turns. |

### 2.2 Semantic conflicts with the design summary

These are not capability gaps, but each one needed a decision. They are now decided (§14.1, Q1–Q3):

1. **`stuck` means two different things.** Today `limits.stuck` is a **duration**: no
   agent-authored channel event for the window, default 10 min (`DEFAULT_STUCK`; `Stuck::After(Duration)`).
   The design defines stuck as "open set unchanged for two consecutive rounds". In a review loop
   every turn writes an agent event, so the existing detector would never fire.
2. **Cost is estimated, not reported.** Fleet cost is computed as `tokens_used × price_per_1k`.
   When no model is priced it falls back to a deliberately high `DEFAULT_PRICE_PER_1K = 0.05`, and
   it uses `EST_TOKENS_PER_MEMBER_ITERATION` as a forward estimate (`loop_run/mod.rs`). Agents do
   not "report cost". The design's rule, "if any agent does not report cost → auto unavailable",
   needs a concrete definition.
3. **Esc already has a meaning in MURMUR.** `esc_action` (`app/keys.rs`): the first press only
   *arms*. A second press within `ESC_DOUBLE_WINDOW` (500 ms) cancels and restores the stream, or
   clears the input. The design gives single Esc a new meaning ("pause after current turn").

### 2.3 Proposed adjustment — thin "review session" adapter over existing mechanisms

The adjustment stays fleet + channel based, as decision A requires. It is **not** a client-side
orchestrator: the loop driver lives in `mur-core` next to `loop_run`, outside both agents, as fleet
design A1 requires. MURMUR is only an attached view and input surface. The driver owns no state
that is not on the channel.

- **A1. Ephemeral = normal fleet + marker, retained channel.** A review session creates a normal
  2-member fleet named with the fixed prefix `review-` (e.g. `review-<id>`) and carrying an
  `ephemeral` marker. At session end the **fleet definition** is removed but the **channel is
  retained**, so replay and audit survive. Retained channels are **not** garbage-collected
  automatically (§7.1). `mur fleet delete` behaviour for ordinary fleets is unchanged.
- **A2. A review-loop driver beside `loop_run`, not inside it.** It reuses the existing pieces
  unchanged: `fleet_bounds`, `check_guards` (fed execution time), the budget helpers, the
  `control::is_stopped` check, `fleet_billing`, and the channel service. It replaces the
  router/DAG iteration with a two-party turn protocol (§3). It does not change `run_guarded`.
- **A3. Pause/resume as channel events.** `paused` and `resumed` are written as signed channel
  events carrying the reason. Resume rebuilds all state (round, ledger, execution-time clock, cost
  so far, mode) by replaying the channel. No side file is authoritative. When replay cannot rebuild
  state, the session never continues from partial state. It degrades as described in §8.2.
- **A4. The kill-switch is checked per turn, not per iteration — for the review driver only.**
  - **Review driver:** checks `.stopped` before every A2A send and at every pause/resume boundary.
  - **Ordinary (non-review) fleets are unchanged.** `run_guarded` still checks the kill-switch only
    between rounds (`guarded.rs:309`, once per iteration). This spec adds no per-turn check to
    `run_guarded` or to anything other than the review driver.
  - **A stop is not an instant abort.** A stop arriving mid-turn takes effect **when that turn
    returns**. The in-flight A2A call is not cancelled. After it returns, no further send happens.
    This is the same cooperative semantics as today, at turn granularity.
  - **Placement-independent.** Wherever the driver runs (in the MURMUR process or a child `mur`
    process, §14.1 Q7), it MUST perform these `.stopped` checks itself, so `mur fleet stop
    <session-fleet>` always reaches it. It does not rely on MURMUR to relay the stop.

**If A1–A4 are rejected,** the fallback is to stop and re-brainstorm, **not** to build a MURMUR-side
orchestrator.

## 3. Behaviour — the loop

### 3.1 Roles and turns

- One session = one ephemeral fleet with exactly two members: `main` and `reviewer`. Members are
  existing agents, and their names are resolved through `canonicalize_agent_name`.
- The reviewer **may** run on a different model than the main agent. That is an agent-profile
  matter; the loop requires nothing of it.
- One **round** is: main produces or revises, then the reviewer returns a verdict. Rounds are
  numbered from 1 by the system.

### 3.2 Reviewer verdict

Every reviewer turn ends with a structured verdict:

```
verdict: approve | revise | blocked
findings:            # new findings only; system assigns IDs
  - severity: high | medium | low
    issue: <text>
prior:               # one entry per previously-issued finding ID
  - id: F<n>
    status: open | withdrawn | resolved | disputed
    reason: <text>   # required when declining a human_note (§6)
```

The wire encoding (fenced JSON vs. tool call vs. A2A data part) is left to the builder, but it
**must** be machine-validated.

- Malformed verdict → retry once with a validation hint → still malformed → treat as `blocked`.
- The reviewer prompt instructs it to return `blocked` when uncertain rather than guess.

### 3.3 Finding ledger

- IDs `F1, F2, …` are assigned **by the system** in issue order, never by a model. An ID the model
  invents is ignored, and the finding counts as new.
- Each round the reviewer must give a status for **every** prior finding not yet closed. A missing
  status counts as a malformed verdict (§3.2).
- Closed states: `withdrawn`, `resolved`, closed by `/rule` (§6). Open set = `open` ∪ `disputed`.
- **Round-stuck** = open set (IDs + statuses) unchanged across two consecutive rounds. This is a
  review-loop condition **in addition to** the existing `limits.stuck` duration detector (§3.5,
  Q1 decided).
- `approve` is rejected by the system while any **high**-severity finding is `disputed`. The
  reviewer's only options then are `revise`, `blocked`, or escalation. A medium/low `disputed`
  finding does not block `approve`, but it is listed first in the human summary.
- Every ledger mutation is a signed channel event (§4). The ledger is a pure fold over those events.

### 3.3.1 Round sealing (round 3 decided)

A round is written as **several signed events, appended one at a time**. There is no batch append,
and the round is **not** one atomic channel entry. Crash safety therefore comes from the replay
rule, not from the write:

- **Append order is fixed:** `rebuttal` (if any) → every `finding_issued` → every
  `finding_status` → `verdict` **last**. The `verdict` event is the round's **seal**. (Before
  round 3 the driver appended `verdict` *before* the findings. That order is changed here. The
  branch is unreleased, so no retained channel uses the old order.)
- **Replay rule:** a round counts only if its `verdict` is present. When the log ends inside an
  unsealed round, every event of that trailing round (`rebuttal`, `finding_issued`,
  `finding_status`) is **dropped from the ledger fold**. In particular, a dropped `rebuttal` does
  not increment `reject_count`, so a crash between appends can never count one reject twice
  (AC8). On resume the driver re-runs that round from the main turn.
- **Limits stay monotonic:** dropping an unsealed round's events drops their *ledger* effect only.
  The `cumulative` execution time and cost they carry are still adopted as a lower bound, the
  same fail-closed rule as §8.2 *Limits on rollback*.
- `turn_sent` events are informational and never affect the fold, sealed or not.

### 3.4 Main-agent rebuttal

- For each open finding the main agent answers `accept | reject | partial`, and a `reason` is
  required for `reject` and `partial`.
- Malformed response → retry once → still malformed → `blocked`.
- On `reject`, the reviewer chooses to withdraw, insist (→ `disputed`), or escalate.
- When the same finding is rejected **twice**, the system escalates to the human automatically.

### 3.5 Stop conditions

The loop stops on: `approve`, `blocked`, escalation, `mur fleet stop`, transport failure after
retry (§8), or one of the **three existing limits**:

| Limit | Phase 1 meaning |
|---|---|
| `deadline` | **Execution time only.** Time spent paused is excluded, and so is human-input wait: time the live driver spends blocked on the human (the semi-auto send prompt, tool-approval prompts inside a member's turn). Each `turn_sent` records its `human_wait_ms` so replay rebuilds the same clock. Human-input wait is not a §7.0 pause: no `paused` event is written and the session state does not change. Wall-clock is logged and never used to stop. |
| `stuck` | **Both detectors run; whichever trips first stops the session** (Q1 decided). (a) The existing `limits.stuck` duration: no agent-authored channel event for the resolved window (`Stuck::After`, default `DEFAULT_STUCK` = 10 min), resolved through the normal resolver and unchanged. `Stuck::Off` disables only this detector. (b) Round-stuck (§3.3). The stop reason names which one tripped (`stuck: no activity` vs `stuck: open set unchanged`). |
| `cost_usd` | Sum across **both** members, using the existing cost accounting. |

There is **no round cap**. The existing `LOOP_ITERATION_CEILING` diagnostic ceiling is not a
setting and is out of this spec's concern.

## 4. Channel event contract

All loop state is recorded as signed events on the session channel (`ChannelEvent`,
`mur-common/src/channel.rs`). Logical event types:

`session_started` (members, mode, resolved limits) · `turn_sent` · `verdict` · `finding_issued` ·
`finding_status` · `rebuttal` · `human_note` · `ruling` · `escalation` · `paused` / `resumed`
(reason, execution-time-so-far) · `mode_changed` · `session_stopped` (reason, unresolved findings).
Turn-ending events (`verdict`, `rebuttal`, `paused`, `resumed`, `session_stopped`) also carry
cumulative execution time and cumulative cost-so-far, which §8.2 rollback depends on.
`resumed_from_checkpoint` (§8.2) records N, the damage, and the adopted time and cost.

Requirement: **replaying the channel reproduces the exact ledger, round, mode, and execution-time
clock**, and that fact is the resume path. Replay that cannot meet this requirement degrades as
described in §8.2. It never continues silently.

**Encoding (Q4 decided).** In Phase 1, every logical event above is stored as a **typed payload
inside the existing `EventKind::Note`** (`mur-common/src/channel.rs`). The payload carries a
discriminator (e.g. a `review` type tag plus a schema version) so the fold can tell review events
from ordinary notes. **No new `EventKind` variant is added in Phase 1**, which avoids knock-on
effects on signing and mobile sync. A `Note` without the review discriminator is ignored by the
fold. A `Note` *with* the discriminator that fails to parse is damage (§8.2), not an ignorable note.

**Crate placement (Q5 decided).** CLAUDE.md: "Shared state with its own file format gets its own
crate." In Phase 1 the ledger is read and written only by `mur-core` (driver + MURMUR). So in
Phase 1 the ledger is a **`mur-core` module**. Its schema is kept free of `mur-core`-only
dependencies so it can be lifted out later. **Exit condition:** if **any second crate** needs to
read it (e.g. `mur-agent-runtime`, `mur-daemon`), the ledger moves to its own crate *before* that
crate starts reading it. Reaching into `mur-core` from that crate is not an acceptable shortcut.

## 5. Auto vs semi-auto

- **Semi-auto (default):** the next outgoing hand-off message is prefilled into the MURMUR input,
  and the human edits and sends it.
- **Auto:** requires **one consent per session**. Each send shows a countdown, and any key during
  the countdown drops back to semi-auto.
  - Countdown default 3 s, minimum 1.5 s, never 0. These are configured values, not hardcoded
    literals (CLAUDE.md rule 2). A configured value below the minimum is clamped to the minimum
    with a warning.
- **Forced back to semi-auto and stop** on: `blocked`, escalation, any limit trip, transport failure.
- **Cost gate (Q2 decided):** a member is **cost-computable** iff (a) its A2A task returns token
  usage **and** (b) its model resolves to a price. A member whose billing mode is `Local` or
  `Subscription` (`fleet/billing.rs`) is cost-computable at **$0** and qualifies for auto. If
  either member is not cost-computable, auto is unavailable and semi-auto still works. The UI names
  the agent and the missing item (`usage` or `price`).
  - The `DEFAULT_PRICE_PER_1K` fallback used by fleet estimation does **not** count as "has a
    price" for this gate. It is an estimate for unpriced models, which is exactly the case the gate
    excludes.

## 6. Human intervention (MURMUR)

| Input | Effect |
|---|---|
| plain text | `human_note` injected into the **next** turn of **both** sides. It outranks findings and does **not** close any finding. If the reviewer does not adopt it, the reviewer must give a reason. |
| `/rule <text>` | `ruling`. The findings it relates to are closed, and both sides must comply in later turns. (`/rule` is currently unused: the MURMUR slash table in `app/slash.rs` has no `rule` entry.) |
| `@<agent> <text>` | Note to that side only. `<agent>` is resolved via `canonicalize_agent_name` (case-insensitive). `@主` = main and `@審查` = reviewer are kept as aliases. |
| `@<unknown> …` | An inline hint next to the input reads: `agent <name> not found; this will be sent as a general note`. It is **never** broadcast silently. Note: `canonicalize_agent_name` returns the input unchanged when nothing matches, so the caller must check existence itself. |
| same finding raised twice in plain text, reviewer still insists | Prompt: `Make this a formal ruling with /rule?` |
| Esc ×1 (**review session only**) | Pause after the current turn completes. The in-flight turn is not aborted (Q3 decided). |
| Esc ×2 within `ESC_DOUBLE_WINDOW` (**review session only**) | Abort generation now. The partial output stays on screen marked `discarded, not sent`. |

**Esc scope (Q3 decided).** The single-Esc = pause binding applies **only while MURMUR is attached to
a review session**. Everywhere else MURMUR keeps today's behaviour unchanged: the first Esc only
arms, and a second Esc within `ESC_DOUBLE_WINDOW` (500 ms, `app/keys.rs`) cancels and restores the
stream or clears the input. For the **whole** review session, MURMUR shows a **persistent** footer
hint, e.g. `Esc pause after turn · Esc Esc abort`, not just on first use. It disappears when the
session ends or MURMUR detaches. A single Esc arms the double-Esc window and also requests a pause.
If the second Esc arrives within the window, the abort supersedes the pause request.

## 7. Attended boundary

- Auto runs only while a MURMUR session is attached to the review session. A close or disconnect
  pauses after the current turn, and state is saved (on the channel, §4).
- On reopen, MURMUR shows `Paused — continue?` and resumes only on explicit confirmation. Resume
  rebuilds state from the channel, and paused time does not count toward `deadline`. If the rebuild
  is incomplete, MURMUR shows the §8.2 prompt instead.
- **Resume command:** `mur fleet review-resume <session>` resumes a paused or crashed session from
  a terminal. It needs a TTY, like `mur fleet review`.

### 7.0 Session states (round 3 decided)

**Concurrency.** Every `mur fleet review` starts an **independent** session named
`review-<id>`. A paused session does not queue, block, or absorb a later review. Session B can run
while session A is paused, and resuming A continues only A's round.

**Pause is not stop.** Their outcomes differ, not only who triggered them:

| Ends by | `session_stopped` written | Fleet definition | Resumable | Stop screen prints `Resume with:` |
|---|---|---|---|---|
| **Pause:** transport failure after retry (§8.1), `q` at the pause prompt, MURMUR closed/disconnected | no | kept | yes | yes |
| **Stop:** `approve`, `blocked`, escalation, a limit, `mur fleet stop`, `replay_failed`, a driver error | yes | removed | no | no |
| **Crash:** process killed (SIGKILL, power loss) with no `paused` event | no | kept | yes (see *Crashed*) | — |

**Run lock.** While a driver runs a session it holds an **exclusive OS advisory lock**
(`flock`, via the existing `fs2` dependency) on `driver.lock` in the session's channel directory,
for the whole life of the driver. The kernel releases the lock when the process exits for any
reason, including SIGKILL. Liveness is decided **only** by whether the lock can be acquired, never
by a stored pid:

- This makes pid reuse irrelevant. A new process that happens to get the dead driver's pid does
  not hold the lock, so the session is correctly seen as not running.
- The file body records `pid`, the process start time, and the host name. These are **for display
  only** (e.g. "running in pid 4123 since 10:02"). They are never used to decide liveness.
- The name `driver.lock` is distinct from the per-agent `running.lock` (`RUNNING_LOCK`), which
  means something else.
- Limitation: advisory locks are unreliable on some network filesystems. `~/.mur` is assumed to be
  local, as elsewhere in MUR.

**Derived state.** State is derived, never stored as a flag:

| State | Lock | Fleet definition | Last review event |
|---|---|---|---|
| running | held | present | any |
| paused | free | present | `paused` |
| crashed | free | present | anything except `paused` / `session_stopped` |
| stopped | free | absent | `session_stopped` |
| corrupted | free | either | `corrupted` marker (§8.2) |
| orphaned | free | absent | anything except `session_stopped` / marker |

`orphaned` only arises from sessions removed by `mur fleet delete` in pre-release builds before round 3 (see §7.1). It is kept so the classification is exhaustive: without it, "definition gone, no `session_stopped`, no marker" would fall through to Keep and never be collected. The cost is one row here and one in §7.1.

**Crashed → resume.** `review-resume` treats a crashed session like a paused one, with these
differences. **Depends on §3.3.1**, so the two must not be built separately or in the other
order: without the sealing rule, resuming a crash can count a reject twice.

- It resumes at the round after the **last sealed round** (§3.3.1). The unsealed trailing round is
  re-run from the main turn.
- It first appends a signed `paused` event with reason `crashed`, then `resumed`, so the channel
  records the crash.
- Execution time counts up to the **last readable event** before the crash. The gap from that event
  to the resume is not counted. The time and cost of a turn that was in flight when the process
  died were never recorded and cannot be recovered. The resume screen says so in one line.
  This is not a fail-open exception to §8.2: the crash gap is known to contain no work, not
  unverified data. A lock-file mtime is not used as a bound, because `flock` does not touch it and
  the file is written once at driver start.
- Mode reverts to semi-auto. Auto needs fresh consent.

### 7.1 Naming, visibility, retention (Q6 decided)

- **Name:** fixed prefix `review-`. User-created fleets cannot use the `review-` prefix, so the
  prefix reliably identifies a review session.
- **Visibility:** review-session fleets are **hidden from `mur fleet list` by default**. A flag
  (name left to the builder, e.g. `--all` / `--include-review`) shows them.
- **Fleet definition:** removed at session **stop** (A1). It is kept on pause and on crash so the
  session can be resumed (§7.0).
- **`mur fleet delete review-…`:** refused while the run lock is held. Otherwise it first appends
  a signed `session_stopped` with reason `deleted`, then removes the definition. The session then
  becomes an ordinary `stopped` prune candidate.
- **Channel:** retained as an audit record. **No automatic GC.** This follows the repo convention
  for monitors (CLAUDE.md: "There is no automatic GC: `prune --older-than` only takes stopped
  monitors"; `cmd/monitor/prune.rs`).
- **Manual prune:** a manual command (exact command path left to the builder) erases retained
  review channels whose last activity is older than `--older-than <DURATION>`, parsed with the
  existing `limits::parse_duration`. **`--older-than` is required (P2 decided)**, with no default,
  matching `mur monitor prune`. `30d` appears only as an example in the help text. Either
  way, **it never runs on a timer.** It supports `--dry-run`, as `monitor prune` does.
  Command: `mur fleet prune-reviews --older-than <DURATION> [--include-paused] [--dry-run]`.
- **Candidates** (states from §7.0; age = last readable channel event, or the marker's
  `detected_at` for corrupted, whichever is later):

  | State | Default | `--include-paused` |
  |---|---|---|
  | running (lock held) | never | never |
  | stopped | candidate | candidate |
  | corrupted, marker readable | candidate | candidate |
  | corrupted, marker unreadable | listed by path, skipped | listed by path, skipped |
  | orphaned | candidate | candidate |
  | paused | kept | candidate |
  | crashed | kept | candidate |

  Paused and crashed sessions are excluded by default because a paused session may be kept on
  purpose. `--include-paused` together with `--older-than` is the only way to clear them, and is
  the exit for dead sessions whose definition would otherwise never go away. There is no
  per-name `--force`: removing one named session is `mur fleet delete`.
- **Removal order is crash-safe.** For a paused or crashed candidate, prune takes the run lock,
  appends `session_stopped` with reason `pruned`, removes the definition, then erases the channel.
  If prune itself dies midway, the session is left `stopped`, which the next default prune removes.
  If the run lock cannot be taken, the session is running and is skipped.

## 8. Error handling

### 8.1 Transport failure

- A2A send failure or peer offline → one retry after a configured delay → still failing → pause,
  revert to semi-auto, and show the reason.

### 8.2 Replay failure on resume

**Rule: never continue silently from a corrupted or partially rebuilt state.**

Replay is a strict, ordered fold over the session's review events (§4). It halts at the **first**
damaged event and never skips past it. An event counts as damaged when any of these hold:

- it is unparseable or truncated. Note: `ChannelStore::load_events` currently **skips** unparseable
  lines with only a `warn!` (`mur-channel/src/store.rs`). For a review session that skip is **not
  acceptable**. The driver must detect the dropped line itself and treat it as damage. A `seq`
  gap alone is **not** a sufficient detector: `append_event` assigns the next `seq` from the last
  *parseable* event (`store.rs`), so a garbled line followed by later appends leaves no gap.
  **Read path (P4 decided):** `mur-channel` gains a **new, additive** read API (a sibling
  function, or an additional report value from a new function) that returns the events **plus** a
  damage report: the line numbers of unparseable/truncated lines and the events that fail
  signature verification. **`load_events` itself is not changed:** same signature, same
  skip-and-`warn!` behaviour, same results for every existing caller. Only the review driver
  uses the new path (AC11f).
- its signature is present but does not verify (`mur-channel/src/sign.rs::verify_one`), or it is
  unsigned when a signature is required. Note: `verify_log` is "NOT yet wired into `load_events`"
  (sign.rs), so the review fold must call verification itself.
- it is a review-tagged `Note` whose payload fails schema validation, or it is an illegal state
  transition (e.g. a `finding_status` for an ID never issued).

Outcomes:

- **Partial — damage after at least one complete round.** State is rebuilt up to the **last
  complete round N before the damage** (P1 decided: never to a mid-round event), and nothing after
  the damage is applied to the ledger. MURMUR shows
  `Recoverable up to round N; later state cannot be rebuilt.` and offers exactly two choices:
  - **Continue from round N.** The driver writes a signed `resumed_from_checkpoint` event
    recording N, the seq/reason of the damage, and the execution time and cost-so-far adopted
    under *Limits on rollback* (below). It then **restarts round N+1** (see *Restarting round N+1*
    below) in **semi-auto**. Auto needs fresh consent.
  - **Abandon.** `session_stopped` with reason `replay_failed`.

  Nothing else is offered, and there is no default that continues without a choice.

  **Limits on rollback — clock and cost are monotonic (fail-closed).** Rolling back the ledger
  does not refund money already spent or time already used, so `deadline` and `cost_usd` (§9:
  "unchanged", never weakened) never get looser.
  - On rollback to round N, execution time and cost-so-far are **never decreased**. They start
    from the round-N values.
  - If any **readable** event after N reports a higher value, the higher value is used. This
    includes an event that fails signature verification or is otherwise damaged. *Readable*
    means the line parses far enough to extract the numeric field. A line too truncated to yield
    the number contributes nothing. Non-finite or negative values are ignored.
  - Principle: **unverified data may only make limits stricter, never looser.** Taking the
    maximum keeps a forged or garbled value from ever lowering a total.
  - The Continue prompt shows these numbers as a **lower bound**, e.g.
    `Execution time ≥ 4m12s · cost ≥ $0.83 (lower bound; may be higher)`.
  - The values adopted are recorded in `resumed_from_checkpoint`. Any later replay uses the
    recorded values and does not recompute them, so the fold stays deterministic (AC11).
  - The normal limit check runs before the first send of the restarted round. If the adopted
    values already meet `deadline` or `cost_usd`, the session stops with that limit, and no A2A
    request is sent.
  - Prerequisite: so that cost-so-far can be recovered from the log, every review event that ends
    a turn (`verdict`, `rebuttal`, `paused`, `resumed`, `session_stopped`) carries **cumulative
    execution time and cumulative cost-so-far** in its review payload (§4). This is a ledger
    payload field. It is **not** an A2A protocol field.

  **Restarting round N+1 — no new protocol fields.** The driver cannot know whether round N+1 was
  delivered: the evidence is in the damaged segment, and the A2A path has no dedupe (dedupe exists
  only in the bridge, `mur-agent-runtime/src/bridge/dedupe.rs`). So:
  - Continue **restarts round N+1 and treats it as possibly already delivered.** No `resend`
    flag, request ID reuse, or other new field is added to the A2A request, and the receiver is
    assumed to do no dedupe.
  - **Every A2A request in the restarted round N+1** (to `main`, and later to `reviewer`) carries
    the fixed plain-text **restart note** below, placed before the round's normal content. Rounds
    after N+1 do not carry it.
  - In the implementation, the note is a **named constant** (e.g. `REVIEW_ROUND_RESTART_NOTE`).
    It is not an inline string literal (CLAUDE.md: no hardcoded values; AC22).
  - The Continue prompt **warns that the workspace may contain changes from the interrupted
    round**, because rolling back the ledger does not revert files. Prompt text:
    `Warning: rolling back the review log does not revert files. The workspace may contain changes from the interrupted round.`

  Restart note text (normative; must match byte-for-byte):

  ```
  This round was interrupted and restarted. A previous attempt may have reached you. Re-read the current workspace state before responding; do not assume your last-seen state is current.
  ```
- **Fatal — nothing valid** (the first review event is damaged, or no complete round is valid).
  Resume is impossible. The session is marked **corrupted**: a `session_stopped` event with reason
  `corrupted` is appended if the channel accepts appends, otherwise a marker is kept outside the
  channel. MURMUR states that the session cannot be resumed and shows the channel path. There is no
  continue option.
  - **Corrupted marker (fatal case, channel not appendable).** The marker kept outside the channel
    is specified as follows.
    - **Location (fixed).** One file, `corrupted.json`, inside the session's own channel directory,
      next to `events.jsonl`: `<MUR_HOME>/channels/<channel id>/corrupted.json`, where the channel
      directory is the one `ChannelStore` already uses (`root.join(id)`, `mur-channel/src/store.rs`).
      The builder does not choose the path. The filename is a **named constant** (e.g.
      `REVIEW_CORRUPTED_MARKER_FILE`), not an inline literal (AC22). Because the marker is inside
      the channel directory, removing the channel (`ChannelStore::delete`, which removes the whole
      directory) also removes the marker. Prune never leaves an orphan marker behind.
    - **Format.** A single JSON object with these fields: `session` (the review fleet name,
      `review-…`), `channel_id`, `reason` (always `"corrupted"`), `detected_at` (RFC 3339 UTC), and
      `damaged_lines` (the 1-based line numbers reported by the P4 read API, as reported). Each
      fatal resume attempt that cannot append writes the marker again, replacing any earlier one.
      The marker never touches `events.jsonl`. It is the one exception to "the only write" in the
      bullet below, and it does not rewrite damaged data.
    - **Not authoritative (A3).** Resume **never reads** the marker to decide anything. Every resume
      attempt re-derives fatal from the channel by replay (no complete valid round ⇒ fatal), so the
      marker adds nothing for resume. It exists only so list/prune can recognise the session
      without replaying it.
    - **Prune equivalence.** Manual prune (§7.1) treats a present, readable marker the same as a
      `session_stopped(corrupted)` event. For `--older-than`, the session's last activity is the
      **later** of (a) the timestamp of the last readable channel event and (b) the marker's
      `detected_at`. If the marker exists but cannot be parsed, prune does not treat it as
      corrupted and reports it by path. Prune never removes a session based on unreadable evidence.
    - **Marker write fails too.** If the marker cannot be written either, MURMUR still states that
      the session cannot be resumed and shows the channel path. It also states that the session
      **was not marked** and **will not be a prune candidate** until a later resume attempt marks
      it. It never fails silently.
- **Damaged data is never rewritten.** No repair, truncation, or deletion of damaged lines happens,
  in either case. Appending the signed marker events above is the only write. The raw channel stays
  available for investigation. A corrupted session is not resumable, and it can only be removed by
  the manual prune (§7.1).

**Interim behaviour until the §8.2 prompt ships (AC11a–g).** In the current build,
`mur fleet review-resume` on a damaged channel refuses and names the first damaged line number
and the reason. It offers no Continue/Abandon choice. The way out, which the refusal message
states, is:

```
Channel damaged at line <N>: <reason>. This session cannot be resumed yet
(Continue/Abandon is not built). Remove it with: mur fleet delete <name>
Then start a new session with: mur fleet review ...
```

**Depends on §7.1** (`fleet delete` writes `session_stopped` with reason `deleted`). Without it
the deleted session would be `orphaned`, which prune only takes because of the §7.0 orphaned rule,
and the message must not be shipped before that delete behaviour exists. User docs (README, docs
site) carry the same two lines.

### 8.3 Stop screen

- The **stop screen** shows the stop reason (`approve` / `blocked` / `escalation` / which limit,
  including which `stuck` detector / `stopped` / transport failure / `replay_failed` / `corrupted`)
  and **all** unresolved findings (`open` and `disputed`). After an approve, disputed medium/low
  findings are listed first.

## 9. Safety triad (unchanged)

- No `MUR_FLEET_AUTORUN` is needed, because runs are attended. The daemon never starts a review
  session.
- `limits` must still resolve through the existing resolver. A session whose limits cannot resolve
  does not start.
- `mur fleet stop <session-fleet>` halts the loop when the in-flight turn returns, wherever the
  driver runs (§2.3 A4). Ordinary fleets keep per-iteration checking.
- The HITL gate inside each member's turn is unchanged.

## 10. Goals / success measure

- G1: A human can run a main↔reviewer loop to a terminal state without copy-pasting between agents.
- G2: Every stop is explainable from the channel alone (reason + unresolved findings), verified by
  replay in tests.

> No baseline metric exists; success is judged by the acceptance criteria below, not by a number.

## 11. Acceptance criteria

**Precondition**
- AC0: §2.3 (A1–A4) is approved, or amended and approved, by a human before build starts.

**State machine (unit, fake transport)**
- AC1: approve path terminates with reason `approve`. Revise path loops. Blocked terminates with `blocked`.
- AC2: paused → resumed continues at the same round with the same ledger.
- AC2a (unsealed round, §3.3.1): given a log whose last round has `rebuttal` with `reject` for F1
  appended but no `verdict`, replay yields `reject_count(F1)` equal to the value before that round,
  and no `escalation`. Resuming re-runs that round from the main turn. After a second `reject` of
  F1 is sealed, exactly one `escalation` exists (AC8 counted once, not twice). The same holds when
  the log ends after some `finding_issued` / `finding_status` events but before `verdict`: none of
  them is in the ledger. A test also asserts the live append order ends with `verdict`.
- AC2b (unsealed round, limits): in the AC2a log, the execution time and cost carried by the
  dropped events are still adopted as a lower bound (they never decrease on resume).
- AC3: each of `deadline`, `cost_usd`, `stuck: no activity` (duration), and `stuck: open set
  unchanged` (round) trips and stops with that limit and detector named. When both stuck conditions
  are armed, the first to trip wins, and setting `limits.stuck` to off disables only the duration
  detector.
- AC4: `deadline` does not trip from time spent paused. Given deadline 10 s and 60 s paused plus
  5 s executing, the loop has not stopped.
- AC4a: `deadline` does not trip from human-input wait. Given deadline 10 s and 60 s at each send
  prompt plus 5 s executing, the loop has not stopped; replay subtracts the recorded
  `human_wait_ms` from execution time.
- AC5: a malformed verdict is retried once. A second malformed verdict yields `blocked`. Same for
  the main-agent response.
- AC6: `.stopped` set during a turn → that turn's A2A call is **not** cancelled. After it returns,
  no further A2A send occurs. This holds whether the driver runs in-process or as a child
  process.
- AC6a: regression — an ordinary (non-review) fleet run under `run_guarded` still checks
  `.stopped` only between iterations. The existing `loop_run` tests pass unchanged.

**Ledger (unit)**
- AC7: IDs are system-assigned and sequential. Model-supplied IDs are ignored.
- AC8: the same finding rejected twice produces an `escalation` event.
- AC9: stuck fires after exactly two consecutive rounds with an unchanged open set.
- AC10: `approve` with a `disputed` high finding is refused. With only a disputed medium/low it is
  accepted, and those findings are listed first.
- AC11: replaying the channel events reproduces the in-memory ledger byte-for-byte (property test
  over random event sequences). Review events are `EventKind::Note` with the review discriminator,
  and no new `EventKind` variant exists.
- AC11a (replay, partial): given a valid log for rounds 1..3 followed by damage in round 4 —
  separately for (i) an unparseable or truncated line, (ii) a bad signature, (iii) a schema-invalid
  review payload — replay yields state equal to the clean replay of rounds 1..3, and reports
  "recoverable up to round 3". No event from round 4 (the partial round) is applied. Continue
  writes `resumed_from_checkpoint` and enters semi-auto.
  Abandon writes `session_stopped(replay_failed)`.
- AC11b (replay, fatal): given damage in the first review event, or no complete valid round, resume
  is refused, the session is marked `corrupted`, and no continue option is offered.
- AC11c (no tampering): in AC11a and AC11b, every byte of the events file before the run is
  unchanged afterwards. Only marker events are appended.
- AC11g (fatal, channel not appendable): given a fatal-damaged review session whose `events.jsonl`
  is made non-appendable (e.g. file mode read-only, channel directory still writable; the test must
  not run as root):
  (a) `corrupted.json` appears at `<MUR_HOME>/channels/<channel id>/corrupted.json` with `session`,
  `channel_id`, `reason = "corrupted"`, `detected_at`, and `damaged_lines` equal to the P4 report.
  (b) Resume is still refused with no continue option. Deleting or editing the marker does not
  change that outcome, which shows resume never reads the marker.
  (c) `prune --older-than <d> --dry-run`, with a cutoff the session passes, lists the session as a
  candidate and erases nothing. A real prune then removes the channel directory, including the
  marker, leaving no orphan file.
  (d) With the channel directory also made non-writable, resume is refused and MURMUR states that
  the session was not marked and will not be a prune candidate. Prune with `--dry-run` does not
  list it.
- AC11d (rollback is fail-closed on limits): given the AC11a logs, where readable post-round-3
  events (including one with a bad signature, and one with a schema-invalid payload whose numeric
  field still parses) report cost and execution time higher than round 3's, after Continue:
  cost-so-far **≥** the highest cost reported by any readable post-N event, and execution time
  **≥** the highest execution time reported by any readable post-N event. Neither value is below
  its round-3 value. A post-N event reporting a *lower* value than round 3 does not lower either
  total. The values adopted are recorded in `resumed_from_checkpoint`, and replaying that log
  reproduces them exactly. The Continue prompt labels them as a lower bound. If the adopted cost
  already meets `cost_usd`, the session stops on that limit and no A2A request is sent.
- AC11e (restart of round N+1): after Continue from round 3, every A2A request of round 4 contains
  the restart note from §8.2 **byte-for-byte**, sourced from a single named constant (a grep finds
  the text in exactly one non-test source location). Requests in round 5 and later do not contain
  it. No new field (e.g. `resend`) is added to the A2A request schema. The Continue prompt shows
  the workspace warning from §8.2.
- AC11f (P4 non-regression): `ChannelStore::load_events` has an unchanged signature, and for a log
  with garbled and badly signed lines it returns the same events as before this change (existing
  tests unchanged and passing). The new read API reports the 1-based line numbers of the garbled
  lines and the events that fail verification for the same log.

**Integration (two stub A2A agents)**
- AC12: a full loop runs to `approve`.
- AC13: the loop is stopped by a limit, and the stop screen data lists all open + disputed findings.
- AC14: transport failure → exactly one retry → pause + semi-auto, with the reason recorded on the channel.
- AC15: session end removes the ephemeral fleet definition and **retains** the channel (A1).
- AC15a: a review fleet is named `review-…`, is absent from default `mur fleet list` output, and is
  present with the show flag. Creating a user fleet named `review-x` is refused.
- AC15b: prune with `--older-than` removes only stopped or corrupted review channels older than the
  cutoff. A running or paused session older than the cutoff is **not** removed. `--dry-run` erases
  nothing. No background or timed prune exists.
- AC15c (crashed, §7.0): a driver killed with SIGKILL mid-round leaves the session `crashed`
  (lock free, definition present, no `paused`). `review-resume` accepts it, appends `paused`
  (reason `crashed`) then `resumed`, continues at the round after the last sealed round, and starts
  in semi-auto. While a driver holds the lock, `review-resume` refuses with "running". A test
  holds the lock from another process with a different pid and shows the decision does not read
  the stored pid (pid reuse cannot fake liveness).
- AC15d (`--include-paused`, §7.1): with sessions in every §7.0 state, all older than the cutoff,
  default prune removes exactly stopped, readable-marker corrupted and orphaned. With
  `--include-paused` it also removes paused and crashed, each first gaining `session_stopped`
  (reason `pruned`). A running session (lock held) is removed in neither case. A paused session
  newer than the cutoff is kept with the flag. `--dry-run` lists the same set and erases nothing.
- AC15e (`fleet delete`, §7.1): `mur fleet delete review-x` on a paused session appends
  `session_stopped` (reason `deleted`) before removing the definition, and the next default prune
  removes it. With the lock held it refuses. Deleting a non-review fleet is unchanged.
- AC15f (damaged channel, interim §8.2): `review-resume` on a damaged channel refuses, names the
  line and reason, and prints the `mur fleet delete` exit. Following it, then running default
  prune past the cutoff, removes the session.

**Manual (MURMUR) — QA script**
- AC16: the countdown shows the configured value and refuses < 1.5 s. Any key reverts to semi-auto.
- AC17: inside a review session, Esc ×1 pauses after the turn and Esc ×2 aborts and shows
  `discarded, not sent`. The footer hint is visible throughout the session, not only after first
  use. Outside a review session, Esc behaviour is unchanged (existing `esc_action_tests` pass, and a
  manual check confirms that a single Esc only arms).
- AC18: `/rule` closes the related findings, and the next turns comply.
- AC19: `@pmm` shows the not-found hint and sends a general note. `@QA` resolves to `qa`.
- AC20: close MURMUR mid-session → reopen → `Paused — continue?` → continue. State is restored and
  the paused time is not counted.
- AC21: auto is refused when a member is not cost-computable (§5), and the message names the agent
  and the missing item. A member with `Local`/`Subscription` billing **is** allowed auto at $0.

**Repo rules**
- AC22: no new source file > 800 lines. Timings, retry delay, and countdown values are config or
  constants, not literals. User-facing text uses `MUR`.
- AC23: docs checklist covered — `README.md`, docs site, product page, and `docs/architecture/fleet.md`
  (new session kind) are updated, or explicitly marked N/A by GitHub Manager.

## 12. Verification plan

As in the approved summary. It maps to AC1–AC11c (unit, including AC6a as a regression),
AC2a–AC2b (unit), AC12–AC15f (integration) and AC16–AC21
(manual). Lint per CLAUDE.md: `cargo clippy --all --all-targets --no-deps --locked -- -D warnings`
and `cargo fmt --all -- --check`.

## 13. Non-goals (Phase 1)

- Web Chat transport. Only A2A + MURMUR.
- Unattended runs / `--unattended` (future: requires `MUR_FLEET_AUTORUN=1`, resolvable limits, and
  explicit consent).
- Daemon-triggered review sessions.
- More than two members, multiple reviewers, or reviewer voting.
- A round cap or any limit beyond `deadline / stuck / cost_usd`.
- Hub GUI surface for review sessions.
- Changing `run_guarded` (including its per-iteration kill-switch cadence), `fleet delete`, or
  existing `limits.stuck` semantics for ordinary fleets.
- A new `EventKind` variant, or any change to channel signing or mobile sync (Q4).
- Wiring `verify_log` into `ChannelStore::load_events` for all channels. The review fold verifies
  its own events, and the global read path is the existing v3d-2 deferral.
- Changing Esc behaviour in MURMUR outside a review session.
- Automatic or scheduled GC of review channels. Repairing damaged channel logs.
- Any MURMUR-side orchestrator (see §2.3).

## 14. Decisions and open questions

### 14.1 Decided (human, 2026-10-04)

- **Q1 — `stuck`.** Keep both: the existing `limits.stuck` duration (no agent activity) **and** the
  round-stuck rule (open set unchanged between rounds). Whichever trips first stops the session.
  → §3.3, §3.5, AC3.
- **Q2 — "reports cost".** Cost-computable = returns token usage **and** the model has a price.
  `Local`/`Subscription` count as $0 and qualify for auto. → §5, AC21.
- **Q3 — Esc.** Inside a review session only, Esc ×1 = pause after this turn. Everywhere else the
  existing double-Esc-within-500 ms cancel is unchanged. A persistent footer hint shows for the
  whole session. → §6, AC17.
- **Q4 — Event storage.** Phase 1 stores review events as typed payloads in the existing `Note`
  kind. No new event kind. → §4, AC11.
- **Q5 — Ledger crate.** `mur-core` in Phase 1. It moves to its own crate if any second crate
  (e.g. `mur-agent-runtime`) needs to read it. → §4.
- **Q6 — Naming/cleanup.** Fixed `review-` prefix. Hidden from `mur fleet list` by default (shown
  with a flag). The fleet definition is removed at session end. Channels are not auto-GC'd. A manual
  `prune --older-than` exists, with 30 days as the suggested value. It never prunes a running or
  paused session. → §7.1, AC15a–b.
- **Q7 — Driver placement.** Builder's choice (in-process or child process). Wherever it runs, it
  MUST honour the A4 `.stopped` checks itself. → §2.3 A4, AC6.

### 14.2 Decided in round 2 (human, 2026-10-04)

- **§2.3 A1–A4 — approved.** This approves the direction. The human will read the final §8.2
  text line by line before AC0 opens.
- **P1 — Checkpoint granularity.** Roll back to the last *complete* round N, never to a mid-round
  event. → §8.2, AC11a.
- **P2 — `--older-than`.** Required, no default. `30d` appears only in help text, matching
  `mur monitor prune`. → §7.1, AC15b.
- **P3 — Reserved prefix.** `mur fleet create review-…` is rejected for users. → §7.1.
- **P4 — `mur-channel` read API: in scope, bound to A3.** A3's "never continue silently from
  corrupted state" depends on it. It is a new, additive API (or an additional report return) that
  surfaces damaged line numbers and verification failures. `load_events` stays exactly as it is
  for all current callers, and only the review driver uses the new path. This is the one change
  outside the `mur-core` footprint of Q5. → §8.2, AC11f.
- **Round-2 additions to §8.2 (required before AC0):**
  - *Clock and cost on rollback are monotonic (fail-closed).* Unverified data may only make
    limits stricter. → AC11d.
  - *Restart of round N+1 with no new protocol fields.* A fixed restart note is stored as a named
    constant, and the prompt warns about the workspace. → AC11e.

### 14.3 Decided in round 3 (human, after QA round 1)

- **Round sealing: replay rule, not schema.** Option (a), a single `round_closed` event, was
  rejected because it changes the event schema. Option (b) was chosen: `verdict` is appended last
  and seals the round, and an unsealed trailing round is dropped from the fold. → §3.3.1, AC2a–b.
- **Pause ≠ stop.** Pause keeps the definition and writes no `session_stopped`. Stop removes it and
  writes one. Each `mur fleet review` is an independent session, so there is no queue. → §7.0.
- **Liveness by OS lock, not pid.** This rules out pid-reuse false positives. → §7.0, AC15c.
- **Crashed sessions are resumable.** This depends on §3.3.1. → §7.0, AC15c.
- **Prune exit for dead sessions: `--include-paused`, gated by `--older-than`.** `--force <name>`
  was rejected because it duplicates `mur fleet delete`. → §7.1, AC15d.
- **`fleet delete review-…` writes `session_stopped`.** → §7.1, AC15e.
- **Interim exit for a damaged channel until AC11.** → §8.2, AC15f.
- **Crash gap is not charged to the deadline.** Time counts to the last readable event; the
  gap holds no work. Lock-file mtime and a heartbeat were rejected. → §7.0.
- **`orphaned` stays** so prune's classification is exhaustive. → §7.0, §7.1.

## 15. Hand-off (after AC0)

1. **Coding agent** builds §3–§8 against §11, including the additive `mur-channel` read API
   (P4). Done = AC1–AC15f green (including AC2a–b, AC6a and AC11a–g), AC22 satisfied, clippy/fmt clean.
2. **QA** runs AC1–AC21. AC16–AC21 are run manually in MURMUR, with real output recorded. For
   AC11a–g, QA also hand-corrupts a real retained channel (truncate, flip a signature byte) and
   confirms the prompts in §8.2, including the lower-bound limits, the workspace warning, and the
   restart note on round N+1. Done = a pass/fail report per AC, with no AC skipped.
3. **GitHub Manager** reviews the PR against this spec, checks AC23 (docs checklist), and merges.
   No release is implied by this spec.
