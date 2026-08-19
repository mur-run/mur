# Unattended HITL: Defer, Don't Time Out

**Status**: P0 shipped (#993). P1a shipped (#1000), P1b shipped (#1001).
**P2 withdrawn as specced** — the original design cannot be built safely; see Layer 2.
P3 designed, not started, and blocked on data that does not exist yet.
**Issue thread**: system-audit follow-on (PR series #986–#991); field report: unattended fleet runs burn the 300 s HITL window and fail, and the request dies with the run.

## Problem

MUR's HITL gates are correct in direction (fail-closed, hash-pinned, signed) but
modeled on an attended operator. Two layers, same assumption:

1. **DAG executor gate** (`mur-core/src/hitl/gate.rs`): writes a `HitlRequest`
   channel event, then **polls for a `HitlResponse` for 300 s** and denies on
   timeout. The step fails, the run fails, and — because `hitl_id` is minted
   fresh per call — the request the human eventually sees is already dead. A
   later approval has nothing to attach to; the only recovery is a full re-run
   that mints yet another request.
2. **Member-runtime tool gate** (`mur-agent-runtime/src/task_runner.rs`): the
   fleet delegation path passes an empty HITL callback
   (`loop_run.rs`: `|_hitl| {}`), so the request is discarded and auto-denied
   after `hitl.timeout_secs`.

The unattended consequences: every gate burns its full window before failing;
`--loop` re-burns it every iteration; approvals cannot arrive late; and the
operator learns about all of it the next morning from a failed run. The
second-order cost is worse: a gate this painful teaches users to run
`set-mode unrestricted` or demand a blanket `--yes` — the pressure to bypass
is itself a security cost.

There is also a latent truthfulness bug in the no-channel path
(`executor/dag.rs`): `needs_approval` + non-TTY + no `--yes` **skips the step
and reports it successful** (`exit_code: 0`, `StepEventKind::Done`).

## Prior art (checked 2026-08-19)

- **LangGraph** `interrupt()` / `Command(resume)`: a pause is checkpointed
  state, indefinite, survives process death.
- **Temporal** signal + durable timer: the official pattern is an escalation
  ladder (notify → remind → escalate → expiry default), i.e. timeout is a
  business decision, not an infrastructure constant.
- **OpenAI Agents SDK** `needsApproval` (optionally a dynamic predicate):
  run pauses, state serializes for days, `approve()`/`reject()` then resume.
- **AWS Bedrock** Return of Control: the pending call is handed back up the
  delegation chain with an invocation id.
- **A2A v0.3**: `input-required` is a standard **non-terminal** task state; the
  client re-engages the same task later. MUR's channel already models exactly
  this (`ChannelState::InputRequired`), so defer is protocol-aligned.
- **OWASP Agentic (ASI06)**: least privilege, tiered autonomy, HITL only for
  consequential actions, and explicit warnings about approval fatigue.

Convergent shape everywhere: **a pending approval is durable state, not a
countdown**, and the human's attention is the scarce resource the architecture
must budget.

## Design: a three-layer funnel

Every risk-tiered action flows through three layers; each layer exists to
spend less of the next layer's budget. Consent is never removed — it moves to
the time and granularity where a human can actually give it.

### Layer 1 — Policy first (consent given earlier) [P1: mode shipped; grants open]

**Shipped (P1a).** `fleet.yaml` declares what an unanswered gate does:

```yaml
hitl:
  mode: defer            # defer | wait | deny   (absent = auto by TTY)
```

`Unanswered::{Defer, Wait, Deny}` (`mur-common/src/hitl.rs`) is a **policy
floor**: every value either tightens the outcome or changes *who waits*. None
of them approves anything, and there is deliberately no `auto_approve` knob —
that is what `--yes` is, and it stays unreachable from unattended fleet paths.
`Deny` short-circuits **before** the resume scan, so a fleet declared free of
risk-tiered work stays that way even if the channel still carries a valid
approval for the same action (locked in by
`deny_mode_outranks_an_existing_approval` and `deny_mode_outranks_yes`).

Why `mode` at all, when P0's TTY detection already picks correctly for the
common cases: a TTY is a proxy for "somebody is watching", and it is wrong in
both directions. A monitored ops fleet running headless has a human on Hub and
wants `wait`; a fleet that must never reach for a person wants `deny` so the
failure is immediate and legible instead of a request nobody will answer.

**Shipped (P1b): tier-keyed standing grants, capped at `write`.**

```yaml
hitl:
  mode: defer
  auto_approve_tiers: [read, write]   # owner takes standing responsibility
```

Decisions recorded from the owner's answers to the two open questions:

- **Grant key = risk tier**, the owner's explicit choice (2026-08-19). It is
  the only key a model cannot widen by rewriting its own input: a command
  pattern is evadable with `git status; rm -rf /`, and step ids rotate as
  workflows change. A tier says what *kind* of action is pre-approved, which
  is exactly the property a standing grant should have.
- **A standing grant may cover `write` and below.** `tier_may_be_granted`
  (`mur-common/src/hitl.rs`) is the hard ceiling: `Spend`/`Destructive`/
  `Privileged` are the actions whose cost a human cannot undo by noticing
  later, and `NetworkEgress` is how data leaves — none behind a config line
  today. Widening the list is a code change with a reviewer, not a YAML
  value.

Enforcement is defense-in-depth, all three layers:

1. `FleetHitl::validate()` refuses an out-of-ceiling tier at `load_fleet` —
   loud, so a user who wrote `destructive` learns it did NOT take effect.
2. `GatePolicy::grants()` re-checks `tier_may_be_granted` inside the gate, so
   a hand-edited fleet.yaml that skipped validation still cannot grant.
3. Ordering inside the Ask tier, strictest first: `Deny` floor → a settled
   human decision for this exact action → the tier grant → park-or-wait. A
   human's explicit "no" therefore outranks a standing grant (locked in by
   `a_human_denial_outranks_a_tier_grant`).

Every grant-driven approval is still **audited**: the gate writes the
request+response pair to the channel with `surface: "policy"` and a reason
naming the pre-approved tier, so "what did this unattended run do without
asking me?" stays answerable after the fact.

Approval TTL stays a constant (7 days, `HITL_APPROVAL_TTL_SECS`): the pin
already bounds *content* staleness, and a configurable clock is a knob with
no demand behind it.

### Layer 2 — Divert, don't block (structure) [P0 park; P2 WITHDRAWN as specced]

For `ask` outcomes:

- **Irreversible actions** (external POST, spend, non-compensable deletes) →
  P0: park immediately. The step is `blocked`, independent branches continue,
  and the run terminates as `blocked(waiting_approval)` — a first-class,
  non-failed outcome that `--loop` treats as "stop, don't burn budget".

#### P2 as originally specced is unsafe — do not build it

The original P2 read: *isolatable actions (file writes) execute speculatively
in an isolated git worktree (the `MUR_PARALLEL_EXEC` machinery); the
`HitlRequest` carries the resulting diff; approval = merge.* Two facts,
verified 2026-08-19, make that unbuildable as written:

1. **The DAG executor has no sandbox.** A command step is
   `tokio::process::Command::new("sh").arg("-c")` (`executor/dag.rs`),
   running inside the `mur` CLI process. The sandbox lives in
   `mur-agent-runtime` — a different process. Workflow shell steps therefore
   run with the user's full privileges.
2. **The worktree machinery is advisory.** `inject_worktree_routing`
   (`cmd/fleet/run.rs`) appends prose to a step's intent — *"you are in an
   ISOLATED worktree, pass cwd=… on EVERY bash call, edit only files under
   it"* — and its own comment says **"Tier 1: no runtime change, best-effort
   isolation"**. It applies only to delegate steps of a `parallel:` fleet.

So: producing the diff requires running the command, and running a
risk-tiered command **before approval, unsandboxed, as the user** is precisely
what the gate exists to prevent. Changing the cwd does not stop
`curl -X POST …`. There is also no typed file-write at this layer to
speculate on — a `Step` carries `command` / `intent` / `delegate_to`, so
"this step only touches the filesystem" is not a decidable property.

#### The safe inversion: gate the MERGE, not the execution

Keep P2's actual goal — *review bytes, not intentions* — by moving the gate
one step later:

- An isolated track's agent does its work and commits **inside its own
  worktree**. That work was authorized when the track was created.
- What needs approval is **landing it**: the gate goes on the merge
  (`mur fleet merge` / cherry), where a real diff already exists.
- Nothing unapproved is ever executed, the human reviews actual bytes, and
  pin drift is structurally impossible because the approved artifact *is* the
  change.

**Not scheduled.** The safe version hangs off parallel tracks, which is still
experimental and default-OFF (`MUR_PARALLEL_EXEC=1`). Building a governance
layer for an experimental feature is the wrong order. Revisit when parallel
tracks graduates, or when it is used for real work.

### Layer 3 — The human, asynchronously (escalation ladder) [P0 surfaces; P1 ladder]

`HitlRequest` events already reach the Hub "Needs You" inbox
(`hitl_pending_list`) and phones (the daemon's `watch_channels` broadcasts
`channel.updated` on every event append). P0 adds precise CLI hints
(`mur channel approve <cid> <hitl_id>`) at the point of deferral and in
`mur job/fleet status`. P1 adds the Temporal-style ladder: push (companion /
mobile / Slack) → remind at T+x → per-fleet expiry action (`deny` default,
`escalate` optional).

### The learning loop [P3]

Approval history is training data. When a class of action is approved N
consecutive times, the harvest pipeline **proposes** a standing grant into the
`mur out` inbox. A human review turns repeated Layer-3 labor into Layer-1
policy. Proposals are never auto-activated.

**Not startable yet, and the reason is not effort.** It mines approval history,
and P1b — which creates the grants a proposal would target — shipped on
2026-08-19. There is no history to mine. Building the miner first would mean
tuning `N` and the similarity rule against imagined data, which is how a
heuristic ends up fitted to nothing. Revisit after a few weeks of real
unattended runs, when the channel actually holds decisions to learn from.

## P0 mechanics (this implementation)

### Gate: `Deferred` outcome + durable matching

`gate()` gains a defer mode and, in **both** modes, a resume scan:

1. Compute `action_hash` (tool, input, channel, step, agent — deterministic).
2. **Resume scan**: newest valid `HitlResponse` whose *payload*
   `action_hash` matches, signature verifies per-actor, and age ≤ TTL
   (7 days, `HITL_APPROVAL_TTL`) → return its allow/deny immediately. No new
   request. This is what lets a re-run (or the next loop iteration) pass a
   gate approved overnight — and what stops a denied action from re-asking
   every iteration.
3. **Dedup scan** (defer mode): an unanswered `HitlRequest` with the same
   `action_hash` → return `Deferred` with the *existing* `hitl_id`; do not
   write a duplicate. (Fixes the pile-of-duplicates bug: `hitl_id` is minted
   per call, so matching must be by `action_hash`, never by `hitl_id`.)
4. Otherwise write the `HitlRequest` + `InputRequired` transition (existing
   code) and either poll (wait mode, unchanged 300 s semantics) or return
   `Deferred` (defer mode).

`GateDecision` gains `deferred: bool` (invariant: `deferred ⇒ !allow`).
Mode selection: `DagExecOptions.hitl_defer: Option<bool>`; `None` = auto —
defer when stdin is not a TTY (unattended by definition: daemon ticks,
schedules, cron), wait when interactive. Explicit config lands in P1's
`fleet.yaml` `hitl.mode`.

Security invariants preserved: responses verify per-actor signatures
(v3d-2); the execute boundary still re-verifies the pin (`hitl_drift`
fail-closed); a resumed approval only ever matches the **exact** action bytes
it approved; TTL bounds temporal staleness; `--yes` remains unreachable from
unattended fleet paths.

### Executor: blocked is a first-class outcome

- A `Deferred` gate returns a `StepResult` marked `blocked` — **not** a
  failure: `on_failure` handling does not fire, and the abort path is not
  taken.
- Dependents of a blocked step are transitively marked blocked without
  executing (rank-order makes a direct-dependency check sufficient).
  Independent branches run to completion.
- Terminal accounting: any blocked step and no failed step → the run record
  is `State::Blocked` (which already exists, renders as "blocked", and is
  non-terminal), and the channel **stays** `InputRequired` (the gate's own
  transition; the `Completed`/`Failed` finalizer is skipped).
- The no-channel `needs_approval` skip-as-success path now reports the step
  as skipped-not-approved instead of silently succeeding.

### Loop: stop on blocked

A `--loop` iteration whose run ends blocked stops the loop with the pending
approval ids and the approve command. Rationale: with defer the gate itself is
nearly free, but each iteration still burns router/member LLM calls; looping
against an unanswered gate converts budget into nothing. The human approves,
then re-runs (`mur fleet run` / next schedule tick); the v3c resume cursor
skips completed steps and the resume scan releases the gate.

### Runtime tool gate (fleet members)

Unchanged in P0 (a mid-turn LLM pause is a checkpointing problem — P2 at the
earliest). P0 relies on: requests remain visible via the A2A stream to any
attached surface, and `hitl.timeout_secs` is per-profile tunable. The P1
ladder gives these requests a push surface within their window.

## Rejected

- **Blanket `--yes` reachable from unattended paths** — stays rejected
  (`fleet/run.rs` fail-closed comment; OWASP ASI06).
- **LLM as final approver** — re-introduces the failure class it gates; models
  may deny or escalate only.
- **Adopting an external workflow engine** (Temporal et al.) — the signed
  channel event log already is the durable state; local-first stays.
- **Timeout-deny as the only mechanism** — burns the window, kills the
  request, and teaches users to disable the gate entirely.

## Where this lives in code (P0)

| Concern | Location |
|---|---|
| Defer mode, resume/dedup scans, TTL | `mur-core/src/hitl/gate.rs` — `gate(.., defer, ..)`, `scan_prior`, `within_approval_ttl` |
| Blocked propagation, terminal accounting | `mur-core/src/executor/dag.rs` — `RunOutcome`, `StepResult.blocked`, `StepEventKind::Blocked` |
| Auto mode selection (TTY) | `mur-core/src/executor/dag.rs` — `unattended()`, `DagExecOptions.hitl_defer` |
| Job status truthfulness | `mur-common/src/fleet.rs` — `JobStatus::Blocked` (non-terminal); `cmd/fleet/run.rs` |
| Loop stop-on-blocked | `mur-core/src/cmd/fleet/loop_run.rs` — `LoopStop::AwaitingApproval` |
| Status rendering | `run_status::State::Blocked` (pre-existing), `cmd/job.rs` |

## P0 implementation notes (deviations worth knowing)

- **`PipelineStatus` gained no variant.** A blocked run reports `Skipped` —
  it does not claim success, and no serialized enum changed. The precise state
  lives in the run record (`State::Blocked`), the channel (`InputRequired`),
  and the job (`JobStatus::Blocked`). Add a `Blocked` variant only if a
  consumer needs to tell "skipped" from "blocked" through that type.
- **`JobStatus::Blocked` IS new** and deliberately non-terminal
  (`is_terminal() == false`), because an approval resumes the job. Without it
  a blocked fleet run fell through to the `Ok(_)` arm in `cmd/fleet/run.rs`
  and was recorded `done` — claiming work that never ran.
- **Blocked-ness is inherited per rank**, by checking each step's direct
  `depends_on` against the blocked set before the rank spawns. Ranks execute
  in dependency order, so this transitively covers the subgraph without a
  separate closure pass.
- **`unattended()` is resolved once per rank**, not per step: every step in a
  run must agree on whether a human is watching.
- **The runtime tool gate is unchanged in P0** — see above.

