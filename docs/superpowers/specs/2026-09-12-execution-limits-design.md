# Execution limits: one bounded model instead of fifteen gates

**Status:** Approved in conversation 2026-09-12 (including the safety-triad change in §5); awaiting plan.
**Scope:** `mur-common` (schema), `mur-agent-runtime` (loop guards, stop reasons, tool timeouts), `mur-core` (fleet loop, `mur limits`, murmur settlement), `mur-daemon` (auto-run gate). No protocol change on the wire.

## 1. Problem

A user on a local model, watching a fleet from murmur, was stopped by five
different mechanisms in one evening — none of which named itself:

| What stopped the work | Where it lives | Default | Visible anywhere? |
|---|---|---|---|
| per-task iteration cap | `hitl.max_iterations` (profile) | `None` → 25 | no |
| per-task token budget | `hitl.max_tokens` (profile) | `None` → 750 000 cumulative *input* tokens | no |
| fleet iteration cap | `loop.max_iterations` (fleet.yaml) | `0` → 8 | no |
| fleet stuck detector | hardcoded | 2 consecutive quiet iterations | no |
| MCP tool-call timeout | per server, default | 120 s | no |
| A2A dial read timeout | hardcoded `a2a_dial.rs` | 600 s | not configurable |
| unattended auto-run | `MUR_FLEET_AUTORUN` + `budget_usd > 0` | off | no |

Three things are wrong with the model, not the numbers:

1. **Counting is used as a proxy for progress.** 25 iterations and 750k
   tokens measure nothing real; they guess whether the agent is still
   useful. Proxies fail both ways: they kill long productive work and let a
   short idle loop run to the cap. Worse, `max_tokens` counts cumulative
   *input* tokens, and every iteration re-sends the whole context, so it
   grows quadratically with steps — a 30-step task with a 25k context is
   750k. rustsmith died at iteration 17 because its context got long, not
   because it did too much.
2. **The real axis is attended vs unattended, not local vs cloud.** When a
   human is watching murmur, the human is the guard: progress is visible
   and `Esc` stops it. Mechanical guards only earn their place when nobody
   is watching — and even then the right guards are time, progress and
   (when it exists) money, not step counts.
3. **Every layer adds its own smaller cap.** fleet 8 × agent 25 × tokens ×
   MCP 120 s × dial 600 s. The user raised two of these to 800 and
   1 000 000 and was stopped by a third. Nobody can reason about the
   product.

Also: `hitl.max_iterations` lives under `hitl:` and has nothing to do with
a human in the loop; `max_tokens` collides with the LLM's max output
tokens. The names say these were bolted on.

## 2. Decisions

| # | Decision | Rejected alternative |
|---|---|---|
| D1 | **The scope the user launched owns the budget; inner scopes inherit and never add a smaller one.** A task delegated by a fleet run has no cap of its own. | Keep per-task caps as a "safety net" under the fleet cap — that is the product-of-caps problem restated. |
| D2 | **Three knobs, one schema, three scopes** (`config.yaml` → `fleet.yaml` → profile/task): `deadline`, `stuck`, `cost_usd`. Iteration and token caps are removed as user-facing settings. | Rename and document the existing six knobs — leaves the proxy model in place. |
| D3 | **Attended runs have no hard stops.** murmur shows elapsed / steps / tokens live; `stuck` only warns; `Esc` is the stop. | Apply the same caps attended and unattended "for consistency" — punishes the case where a human is already the guard. |
| D4 | **Unattended runs must be bounded, and a bound is `deadline` OR `cost_usd`.** A local model (`BillingMode::Local`) has no `cost_usd` knob at all, so its bound is a deadline. | Keep "positive `budget_usd` required" — forces local users to invent a dollar figure that means nothing (see §5). |
| D5 | **Stuck measures progress, in minutes.** Stuck = N minutes with no file write, no channel event, and no tool call whose arguments differ from the previous iteration's. Default 10 min. | Keep "2 iterations without agent activity" — an iteration is not a unit of time or progress. |
| D6 | **Long-running tools return a handle, never block.** `parallel_jobs` and `fleet_run` return `run_id` at once; progress comes from `mur_job_status` (already exists). The MCP call timeout stays short because nothing legitimate needs it long. | Raise the MCP default from 120 s — the next tool that takes 20 minutes hits it again. |
| D7 | **Liveness is heartbeats, not a bigger read timeout.** The runtime emits a heartbeat frame while the model is thinking; the dial's idle timeout stays short and never fires on a thinking router. | Raise `DEFAULT_DIAL_IO_TIMEOUT` past 600 s — same failure, later. |
| D8 | **Every stop names its reason and its remedy, where the user is looking.** `LoopStop` already has the reasons; they reach the settlement card and the fleet rail, not only `progress.json`. | Point users at `progress.json`. |
| D9 | **Authorization gates are untouched** (`fleet_run` allowlist, `parallel_jobs.targets`, tool Ask/Deny, path grants) — they are security, not budget. Two behaviours change: a missing capability fails **at dispatch**, and authorization errors are **non-retryable** so the model cannot spin on them. | Fold authorization into `limits:` — conflates safety with cost. |
| D10 | **`mur limits <agent\|fleet>` prints every knob's effective value and its source.** | A doc page listing defaults — stale the day it ships. |

## 3. The model

### 3.1 Schema (identical at every scope)

```yaml
limits:
  deadline: 2h        # wall clock for the whole unit of work. Duration string.
  stuck: 10m          # minutes of no progress before stopping (unattended)
                      # or warning (attended). `off` disables.
  cost_usd: 5.00      # ONLY when the model's BillingMode is UsageBilled.
                      # Absent/ignored for Subscription and Local.
```

Resolution: `config.yaml limits:` → `fleet.yaml limits:` → agent `profile.yaml limits:` → per-call override (`--deadline`, `--cost-usd`). Inner scopes **replace** a key; they never combine. A key absent at every scope takes the built-in default (§3.3).

Removed as settings: `hitl.max_iterations`, `hitl.max_tokens`, `loop.max_iterations`, `loop.budget_usd` (renamed `cost_usd`). See §6 for migration.

### 3.2 Attended vs unattended

A run is **attended** when a murmur session holds its task (the same fact `can_approve` already carries for HITL). Everything else — `mur agent send`, cron fires, `fleet_run` from an agent, daemon auto-run, `--loop` — is unattended.

| | attended | unattended |
|---|---|---|
| `deadline` | ignored | **required** to start (built-in default if none set) |
| `stuck` | warns in the live band, never stops | stops with `LoopStop::Stuck` |
| `cost_usd` | ignored | enforced when present and billable |
| hard stop | `Esc` | deadline / stuck / cost / kill-switch |

### 3.3 Built-in defaults

| knob | attended | unattended |
|---|---|---|
| `deadline` | — | 1h fleet run, 30m single task |
| `stuck` | 10m (warn) | 10m (stop) |
| `cost_usd` | — | none (bounded by deadline) |

Defaults are constants in `mur_common::limits`, and `mur limits` prints them labelled `(built-in default)`.

### 3.4 Where the budget lives per unit of work

| unit | owner of `limits` | inner scopes |
|---|---|---|
| murmur turn | attended: none | tools inherit "no cap" |
| `mur agent send` | agent profile → config | — |
| `mur fleet run` / `--loop` | fleet.yaml → config | delegated tasks inherit the fleet's remaining deadline; no per-task cap |
| `fleet_run` tool from an agent | the fleet's | same |
| workflow step | the run's | same |

"Inherit the remaining deadline" means a task delegated with 12 minutes left on the fleet clock gets a 12-minute deadline, not a fresh one.

### 3.5 Stuck detection

Progress signals, any one of which resets the clock:

- a file write through any tool,
- a channel event authored by an agent (message, delegation, tool result),
- a tool call whose `(name, arguments)` differ from the previous iteration's set.

Not progress: another LLM turn that produces only text with no tool call and no channel event (that is the "reasoning in circles" case), a tool call identical to the last one (the "retrying the same thing" case — which is exactly what happened with `parallel_jobs not authorized` ×3).

Attended: at the threshold the live band shows `⚠ no progress for 10m — Esc to stop`. Unattended: `LoopStop::Stuck` with the last three tool calls in the reason.

### 3.6 Timeouts

Two kinds, and they stop sharing a number:

- **Liveness** (is the peer alive?): the runtime emits a heartbeat frame on the A2A stream at least every 30 s while a turn is in progress, including during model inference. The dial's idle timeout becomes 90 s. A router that thinks for ten minutes is alive the whole time.
- **Work duration**: not a timeout at all. `parallel_jobs` and `fleet_run` return `{run_id, status: "dispatched"}` within a second; the caller polls `mur_job_status`. The MCP per-call default stays 120 s and is documented as "a tool that needs longer must return a handle".

### 3.7 Stop reasons reach the user

`LoopStop` gains `Deadline`, `Stuck { last_calls }`, `Cost { spent, cap }` detail, and the runtime's task loop gains the same enum for single tasks (replacing the silent "graceful exit with summary" that today looks like the agent simply had nothing to say). The reason is written as a channel `state-change` event with a `stop_reason` payload, so:

- the murmur settlement card shows a `■ stopped` row: `stopped: deadline 1h — mur fleet limits develop-rust --deadline 3h`,
- the fleet rail shows it under the fleet line instead of `finished (0s)`,
- `progress.json` keeps `outcome` as today.

Every reason carries the one-line remedy. The `finished` word is reserved for `Converged`.

### 3.8 Fail at dispatch, not after the budget

Before a delegated task starts, the runtime checks the task brief's declared needs against the agent's tool policy: a coding task (`write_file`/`edit_file`/`bash` requested or implied by the fleet role) on an agent whose policy denies them fails immediately with `cannot start: rustsmith has no write_file — mur agent perm tool-allow rustsmith write_file`. The check reads the same `ToolRule` list the gate reads; no new source of truth.

Authorization errors (`not authorized for parallel_jobs`, `not in fleet_run.agents`, tool `Deny`) become a distinct `ToolError::NotAuthorized` that the loop treats as terminal for that tool: the model is told once and the tool is removed from its list for the rest of the turn.

### 3.9 `mur limits`

```
$ mur limits develop-rust
scope: fleet develop-rust (unattended when run by daemon/agent; attended in murmur)
deadline   2h     ← ~/.mur/fleets/develop-rust/fleet.yaml
stuck      10m    ← built-in default
cost_usd   —      ← router model is Local; knob does not apply
members inherit: rustsmith, pm, qa, repomanager (no per-task cap)

$ mur limits rustsmith
scope: agent rustsmith
deadline   30m    ← built-in default (unattended single task)
stuck      10m    ← ~/.mur/config.yaml
cost_usd   —      ← model claude_haiku is Subscription; knob does not apply
note: hitl.max_iterations: 800 in profile.yaml is IGNORED since 2.79 — remove it
```

`--json` for the Hub. `mur fleet limits <name> --deadline 3h` and `mur agent limits <name> --stuck off` write the scope's `limits:` block (dual-write pattern where a live agent must pick it up).

## 4. Error handling

- A `limits:` block with an unknown key or an unparsable duration is a load error with the path and line, not a silent default.
- `cost_usd` set on a scope whose model is not `UsageBilled` loads fine and is reported by `mur limits` as "does not apply" — a fleet may mix models.
- Heartbeat missing for 90 s → the dial fails with `agent X stopped responding` (not "went idle"), and the reason names the last heartbeat time.
- A stuck stop always includes the last three `(tool, args-hash)` so the user can see the loop.

## 5. Safety triad — what changes and what does not

The rule "unattended auto-run is OFF unless `MUR_FLEET_AUTORUN=1`, also requires a positive `loop.budget_usd`, kill-switch honoured, governance fail-closed" was set deliberately. Its intent is **no unbounded unattended run**. This spec keeps every property and generalises one:

| property | before | after |
|---|---|---|
| opt-in | `MUR_FLEET_AUTORUN=1` | unchanged |
| bounded | `budget_usd > 0` | **`deadline` set, or `cost_usd` set on a billable model** — at least one, checked in `fleet_tick::due_fleets` exactly where `has_budget` is today |
| kill-switch | `.stopped` sentinel | unchanged |
| governance | fail-closed on Err | unchanged |
| approvals | `yes:false` everywhere | unchanged |

A local-model fleet is bounded by its deadline. A billable fleet with neither knob does not auto-run, same as today.

A billable fleet bounded by a deadline **alone** is allowed — a deadline is a cost bound in a different unit (rate × time), and a dollar cap built on a stale price table is a falser comfort than a clock — but the choice must be made knowingly, never slid into. So (decided 2026-09-12): the loop prints one line at start, `⚠ billable, no cost cap — bound is the deadline (2h); spend is reported per iteration at $<rate>/1k — set --budget-usd to cap it`, and the Hub badge (§10.2) reads `bounded by deadline only · no cost cap` rather than a bare green check. No projection of total dollars is printed: iteration duration is unknown, and a made-up number would be worse than the honest rate. **Approved by the user in conversation on 2026-09-12**; the `feedback_autonomous_loop_safety_audit` memory is updated to match.

## 6. Migration

- `hitl.max_iterations` / `hitl.max_tokens` in a profile: loaded, ignored, and reported once at agent start and by `mur limits` ("IGNORED since 2.79 — remove it"). Not an error: nobody's agent stops starting because of a stale key.
- `loop.max_iterations`: same treatment.
- `loop.budget_usd`: read as `limits.cost_usd` when `limits:` is absent, so existing billable fleets keep their bound; `mur fleet limits` writes the new key.
- The runtime-internal iteration counter stays as a **diagnostic** (it appears in the stop reason and the live band) with an absurd ceiling (10 000) that exists only to turn a runaway bug into a stop instead of a hang. It is not a setting.

## 7. Testing

- `mur_common::limits`: resolution across three scopes, replace-not-combine, duration parsing, `cost_usd` applicability by `BillingMode`.
- Runtime loop: attended run passes the old 25-iteration mark without stopping; unattended stuck stop fires on three identical tool calls and does not fire when a file was written; deadline inherited from the fleet is the remaining time, not a fresh one.
- Fleet loop: `LoopStop::Deadline`/`Stuck`/`Cost` each land as a channel `state-change` with a `stop_reason` and the remedy string; the rail renders it.
- `fleet_tick::due_fleets`: local-model fleet with a deadline auto-runs; billable fleet with neither knob does not; kill-switch still wins. These three replace the current "positive budget" test rather than sit beside it.
- Dispatch preflight: a coding brief on an agent with `write_file` denied fails before the first LLM call.
- `parallel_jobs` returns within 2 s with a `run_id` for a job that takes 60 s.
- `mur limits` snapshot test: source labels per row.
- Migration: a profile with `hitl.max_iterations: 800` starts, warns once, and the value has no effect.

## 8. Out of scope

- Context-window management (compaction) for very long attended sessions — a separate design; this spec only stops the runtime from *killing* such a session.
- Per-tool cost attribution.
- Removing `hitl.timeout_secs` — it is a real HITL knob and stays.

## 9. Rollout

1. Stop reasons reach the settlement card and fleet rail (§3.7) — no schema change, immediate relief.
2. `cost_usd` applicability by `BillingMode` + the §5 gate change — small, isolated, unblocks local users.
3. `mur_common::limits` schema, resolution, `mur limits` (§3.1, §3.9) with migration warnings (§6).
4. Runtime loop switch: caps → deadline/stuck, inheritance from fleet (§3.4, §3.5).
5. Heartbeats + handle-returning tools (§3.6).
6. Dispatch preflight + non-retryable authorization (§3.8).

Each step is its own plan and PR.

## 10. Hub surface

The Hub already has the three places these settings belong — the global
Settings page, the fleet detail's Settings tab (today: trigger / max
iterations / deadline / budget / done-when), and the agent detail's
Overview. No new page. One rule above all: **the Hub renders what the CLI
resolves.** A single Tauri command `limits_resolve(scope) -> LimitsView`
wraps the same `mur_common::limits` resolver `mur limits` uses, and
returns per knob `{ value, source, applies, note }`. The Hub never
re-derives a default or an applicability rule in TypeScript — the two
surfaces disagreeing about "what is in force" is the disease this whole
spec treats.

### 10.1 One `LimitsPanel`, rendered at three scopes

| scope | where | what it edits |
|---|---|---|
| global | Settings → General → "Execution limits" | `~/.mur/config.yaml limits:` |
| fleet | Fleet detail → Settings tab, **replacing** the loop-guards block (max iterations / budget go away; trigger, cron and done-when stay) | `fleet.yaml limits:` |
| agent | Agent detail → Overview, a "Limits" card with Edit (no fifth tab) | `profile.yaml limits:` |

Each row is `knob · effective value · source chip · action`:

```
deadline   2h      this fleet          [Reset to inherited]
stuck      10m     built-in default    [Override here]
cost_usd   —       Local model — no cost cap applies
```

- An **inherited** value renders dimmed with its source chip
  (`built-in default` / `config.yaml` / `this fleet`); its action is
  *Override here*. A **local** value renders solid; its action is *Reset to
  inherited*, which deletes the key rather than writing the parent's value
  (so a later change upstream still flows down).
- `cost_usd` follows D4: shown as an editable row only when
  `applies == true` (the scope's model is `UsageBilled`). Otherwise the row
  is a single line of text naming why, never a disabled input — a disabled
  field reads as "you are not allowed", and the truth is "this does not
  exist for you".
- Duration inputs accept `30m`, `2h`, `1h30m`; validation is the same
  parser as the CLI, exposed through the resolve command's error, not
  reimplemented.

### 10.2 The fleet Overview stat cards

The four cards today read `never / Last auto-run · 0 / Max iterations ·
$2 / Budget · Router decides each iteration / Done when`. They become:

```
never          2h            10m           Router decides…
Last auto-run  Deadline      Stuck         Done when
```

with a fifth element on the header line next to the trigger: **`bounded ✓`**,
**`bounded by deadline only · no cost cap`** (a billable fleet with a deadline
and no `cost_usd` — amber, not green, because the user is choosing to run
without a dollar ceiling and should see that they are), or
**`unbounded — will not auto-run`**. That badge is §5 made visible; the
unbounded state links to the Settings tab. On a billable fleet the third card
shows `$5.00 / Cost cap` instead of stuck when a cap is set; stuck moves to
the panel.

### 10.3 Stop reasons where the Hub user is looking

The Jobs tab and the Overview job rows show the `stop_reason` from the
channel `state-change` event (§3.7) as the row's status —
`■ stopped: deadline 1h` — with one inline action that opens `LimitsPanel`
at the fleet scope with that knob focused. `finished` appears only for
`Converged`, same word rule as the CLI.

### 10.4 Migration warnings and attended state

- A stale key (`hitl.max_iterations`, `loop.max_iterations`,
  `loop.budget_usd` once migrated) shows as an amber row at the top of the
  panel: `hitl.max_iterations: 800 in profile.yaml is ignored — Remove`.
  Remove deletes the key through the same write path as an edit.
- The agent-scope panel says whether a save needs a restart. Fleet and
  global do not (read per run); an agent profile does, and the row uses
  the existing "restart required" affordance rather than a new one.
- The Hub's chat pane is an **attended** surface exactly like murmur:
  while it holds a task, the live band shows elapsed / steps / tokens, the
  `stuck` threshold shows a warning banner with a Stop button, and no hard
  stop fires. Hub chat is in scope for D3.

### 10.5 Wiring notes

- `limits_resolve`, `limits_set(scope, patch)` and `limits_remove_stale
  (scope, key)` are the only three commands; `fleet_set_loop` keeps
  trigger / cron / done-when and drops the guard fields.
- The Hub is workspace-excluded: `LimitsView` is a DTO owned by the Tauri
  side, so a later change to `mur_common::limits` must grep the Hub
  (`gotcha_workspace_excluded_addonref_literals`).
- Hub work is rollout step 3b, after `mur limits` (§9 step 3) has settled
  the shape, and before step 4 flips the runtime — so users can see the
  new model before it starts governing their runs.

