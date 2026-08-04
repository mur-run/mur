# Fleet loop settings: ask questions the user can answer

**Date:** 2026-08-02
**Status:** design, not implemented
**Spans:** `mur-core` (loop semantics, write-boundary validation), `mur-hub-gui`
(Fleet settings form, i18n)

## Problem

The Hub's fleet **Settings** panel asks the user five questions about the loop.
Two of them cannot be answered without knowledge the UI never supplies, and all
five accept values that are silently reinterpreted as something else.

The trigger for this spec was a user configuring a real fleet (`builder`, one
member, cron every 15 minutes) and asking, in order: what does 截止時間 mean,
will it reach iteration 3, what am I supposed to type into 完成條件, and how
would any user know. Every one of those questions is the UI's fault.

### 1. `done_when` is half a contract, and the UI owns the wrong half

`done_when: marker:<TEXT>` converges the loop when an agent emits `<TEXT>` as a
whole line. That only works if something *taught* an agent to emit it — and
that teaching lives in the fleet's goal or in a member's skill, nowhere near
this field. The UI presents a free-text box whose valid contents depend on
prose the user wrote elsewhere.

Empirically nobody completes the contract by hand. Across the 11 fleets in this
developer's `~/.mur/fleets/`, exactly one has a marker — `deep-research`, whose
`marker:RESEARCH_COMPLETE` is written by `cmd/deep_research/run.rs` at creation
time, paired with a `deep-research-router` skill that teaches the router to emit
it, with `cmd/sync_cmd.rs` asserting the skill text still does. The one correct
usage in the codebase is machine-authored on both sides.

### 2. Five inputs are fail-open

Each of these silently becomes something other than what was typed:

| Input | Silently means |
|---|---|
| `done_when: DONE` | router judgment (`done_marker` returns `None` for any non-`marker:` string) |
| `deadline: 2026-12-31` | **no deadline** — `parse_duration` fails, `effective_deadline` yields `None` |
| `trigger: cron:<garbage>` | never fires; `next_fire_after` errors and `is_due` returns false |
| `max_iterations: 0` | **8** — `effective_max_iterations` filters `n > 0`, then falls back to `DEFAULT_MAX_ITERATIONS` |
| `budget_usd: -1` | no budget ceiling — `effective_budget` filters `b > 0` |

The calendar-date deadline is not hypothetical: `mur-common/src/fleet.rs`'s
serde round-trip fixture contains `deadline: '2026-12-31'`, and
`fleetSettingsForm.ts` already carries a comment warning that an unparseable
value "would silently mean *no deadline enforced*". The frontend guards it; the
write boundary does not.

### 3. A job-queue fleet has no way to say "stop when the queue is empty"

`iteration_goal()` claims the oldest queued job as the iteration's goal, or
falls back to the standing goal when the queue is empty. There is no stop
condition for a drained queue, so a cron-triggered worker fleet with nothing
queued re-sends its role description every iteration.

Stuck-detection does **not** catch this. `progressed` is "did any new
Agent-authored event appear this iteration" — a member replying *"please tell me
what to run"* counts, so `stuck` resets to 0 every time. The loop runs to
`max_iterations`, plus one router-convergence LLM call per iteration, on every
cron tick.

### 4. The cron field is a bare text box

Choosing **Cron** hands the user a raw 5-field expression to type by hand, with
no preview, no presets, and no way to tell a working expression from one that
will never fire.

## What already exists

Verified in the tree, not assumed.

- **`cmd_fleet_set_loop` is the single write path** for the `loop:` block. The
  Hub's `fleet_set_loop` Tauri command and `mur fleet set-loop` both call it, and
  its merge semantics ("only fields passed as `Some(..)` are changed") are
  deliberate and documented.
- **`scheduler::next_n_fires(expr, count)`** already returns the next N fire
  times as `chrono::DateTime<Local>` for a 5-field expression, and
  `next_fire_after` is what the daemon uses to decide due-ness. The expensive
  half of a good cron UI is built.
- **`parse_duration` is `pub`** in `loop_run.rs`, and `mur-core` already depends
  on `mur-agent-runtime`, so both parsers are reachable from the write boundary.
- **`done_when` is a plain `String` everywhere.** Only `loop_run::done_marker`
  and `cmd/deep_research/ask.rs` interpret it.
- **`interval:<dur>` already exists** as a trigger kind — MUR's equivalent of
  EventBridge's `rate(15 minutes)`.
- The Hub's `TriggerKind` select is `manual | interval | cron`; the settings
  form is plain React with project CSS classes, no component library.

## Design

Four changes, each independently shippable.

### §1 A third completion policy, encoded in the existing field

The UI should not ask *"what string?"* but *"when is this fleet finished?"* —
a question with three answers a user can actually give:

| Policy | Decided by | Cost |
|---|---|---|
| Router judgment | asking the router DONE/CONTINUE each iteration | one LLM call; can misjudge |
| **Queue drained** | `iteration_goal()` claimed no job | zero LLM, zero agent cooperation |
| Agent declares done | `marker:<TEXT>` matched as a whole line | zero LLM, needs the taught contract |

Only "queue drained" is missing. Add it to `done_when` as the sentinel value
`queue-empty` rather than a new field: the field's meaning is already "what
counts as finished", and three policies are three answers to one question.
Splitting them across two fields would force a combination semantics ("both set
— which wins?") that nothing needs.

Parsing moves into one pure function beside the existing `done_marker`, which
stays as-is for `ask.rs`:

```rust
pub enum DonePolicy<'a> { Router, QueueEmpty, Marker(&'a str) }
pub fn done_policy(done_when: &str) -> DonePolicy<'_>
```

`marker:<TEXT>` → `Marker`, `queue-empty` → `QueueEmpty`, everything else
(empty string, legacy values like `all_tasks_done`) → `Router`.

**Enforcement point:** immediately after `iteration_goal()` returns and before
`plan_via_router()`. Policy is `QueueEmpty` and no job was claimed →
`break LoopStop::QueueDrained`. This sits ahead of every LLM call, so a
cron tick on an empty queue costs nothing rather than costing little.

Add `LoopStop::QueueDrained` and its `stop_reason_label` arm ("queue-drained").

**Edge case:** an empty queue at iteration 0 stops immediately having done
nothing. That is correct — "nothing queued, nothing to do" — but must print a
clear line, or it reads as a failure.

**Backward compatibility:** an older `mur` reading `queue-empty` falls through
to router judgment, which is today's behaviour, still bounded by the iteration
cap and budget. Degrades to the status quo, not to something dangerous.

**Test:** `done_policy` parsing over four inputs (marker, `queue-empty`, empty,
arbitrary legacy string). The break site is a three-line `matches!` branch; an
integration test would have to run agents and is not worth it.

### §2 The Hub offers policies, not strings

Replace the free-text `done_when` input with a select:

| Option | Writes |
|---|---|
| Router decides (default) | `""` |
| Stop when the job queue is empty | `queue-empty` |
| Agent declares done (`marker:XXX`) | the loaded value, unchanged |

The third option appears **only when the loaded value is already a marker**. The
Hub never creates a new one. The justification is not "the user might typo" —
§3 makes a malformed value impossible to *write* through either front door — but
that **the Hub cannot supply the contract's other half**. Whoever needs a marker
is authoring a goal or a skill that teaches it, and is therefore already editing
YAML.

Rejected alternative: have the backend inject *"when finished, emit `<TEXT>` on
its own line"* into each iteration's goal, so the contract is derived from one
source. `parse_router_plan` prepends the goal verbatim to **every** member's
intent (`{goal}\n\n[Router assignment for X]: {task}`), so the instruction would
reach every member, and `channel_has_marker` accepts **any** Agent-authored
event — member X finishing its own step would converge the whole fleet while
others are still working. The current design's safety rests on a human
deliberately teaching the marker to the right agent; auto-injection removes
exactly that deliberation. `channel_has_marker`'s own doc comment reasons at
length that stopping early on a false positive is the worse failure.

Switching away from the marker option and saving clears it — including for
`deep-research`. Accepted: it is a labelled, deliberate action, and guarding it
would mean a disabled-but-selected state for one rare misclick.

**A latent bug this fixes:** `doneWhen.trim() || null` sends `null` for an empty
string, and `settings.rs` treats `None` as "leave alone" — so **the Hub cannot
currently clear `done_when` at all**. The select's "Router decides" must write
`""` explicitly.

**Normalization:** a legacy value like `all_tasks_done` loads as "Router
decides" (which is what it does) and is normalized to `""` on the next save.
Deliberate — the string was already lying.

**Reverts:** the `marker:` prefix check added to `settingsAreValid` (and its two
tests) and the `doneWhenHelp` hint both describe a free-text field that ceases
to exist. The `type="number"` inputs and `Math.trunc` on `maxIterations` stay.

### §3 Validate at the write boundary

Add a pure validator to `settings.rs`, called by `cmd_fleet_set_loop` before it
touches the store. It covers all five fail-open inputs from the Problem section,
reusing the real parsers — `loop_run::parse_duration` and
`scheduler::next_fire_after` — so validation cannot disagree with execution.
Error messages name the fix (`done_when must be queue-empty or marker:<TEXT>`).

Three boundaries:

**Validate only the fields passed in this call, not the merged block.** A fleet
already holding `deadline: 2026-12-31` must not have `mur fleet set-loop
--budget-usd 2` rejected because of it. This matches the merge semantics the
function already documents.

**Reads stay lenient.** Existing fleets carrying `all_tasks_done` keep loading;
unknown values keep falling back to today's behaviour. Fail-closed on load
would break existing configs for no safety gain — the loop's caps bound
everything regardless.

**Reject values that will be reinterpreted; do not reject combinations that are
merely unusual.** `cron:` + `budget_usd: 0` means "will never auto-run" — odd,
already warned about in the UI, and the user's business. Only a value whose
meaning gets silently rewritten is an error.

Programmatic construction (`deep_research/run.rs` builds a `FleetLoop` and saves
directly) does not pass through here and does not need to. The boundary guards
user input, not internal construction.

**Test:** one table-driven test over the five fields, valid and invalid cases,
plus one asserting a partial update is not blocked by a pre-existing bad value.

### §4 Cron input: presets above, verification below

Survey of how shipping products handle this:

| Product | Approach |
|---|---|
| n8n | Interval-unit dropdown (seconds…months) with progressively revealed hour/minute/weekday fields; "Custom (Cron)" is the escape hatch, and the docs send you to crontab.guru |
| AWS EventBridge Scheduler | Three schedule types: `rate(15 minutes)`, cron, one-time |
| Google Cloud Scheduler | unix-cron **plus** groc (`every 5 minutes`, `first sunday of month 12:00`) — but groc is CLI/API only and cannot be entered in the console |
| Vercel | Raw cron, no assistance |
| react-js-cron | Per-field dropdown builder, but requires antd ≥6 as a peer dependency |
| cronstrue | Humanized description, zero deps, MIT, has a `zh_TW` locale — but explicitly cannot compute next run times |

The common principle: a friendlier layer sits above cron, and cron is the escape
hatch. The instructive failure is Cloud Scheduler's — groc is the better format
and never reached the console, so in practice it does not exist. MUR is one step
from the same mistake: `interval:<dur>` is already the friendly layer, and the
Cron mode still drops the user into a bare text box.

**Design: keep the raw expression field as the single source of truth, add a
preset filler above it and a fire-time preview below it.**

*Above:* a "schedule shape" select paired with a native `<input type="time">`,
composing three shapes from the picked time `H:M` — hourly (`M * * * *`, using
the minute only), daily (`M H * * *`), weekdays (`M H * * 1-5`). Those three
plus the existing `interval:` mode cover the realistic fleet schedules; "third
Tuesday of the month" is left to editing the expression directly. The user never
has to *compose* a cron, only pick one and optionally adjust it. Picking a shape
overwrites the expression field; typing in the field afterwards is free and does
not reset the select.

*Below:* the next three fire times, from the existing
`scheduler::next_n_fires`, via a thin Tauri command
(`cron_preview(expr: String, count: usize) -> Result<Vec<String>, String>`
returning pre-formatted local-time strings), debounced 300 ms on input:

> Next: 8/2 21:00 · 21:15 · 21:30 (local time)

**No structured builder.** react-js-cron would drag in an entire UI framework
for one field; hand-rolling n8n's progressive disclosure is six units of
conditional fields for a need three shapes cover.

**No cronstrue.** Concrete times carry the meaning (21:00 · 21:15 · 21:30 *is*
"every 15 minutes"), at zero new dependencies and zero new translated strings.

**The preview must call Rust, not a JS cron library.** A JS parser can disagree
with the `cron` crate on six-field padding or day-of-week numbering, and a
preview that disagrees with the scheduler is worse than no preview. Reusing
`next_n_fires` guarantees the preview and the daemon answer from the same
engine.

This also catches "valid syntax, never fires" (`0 9 31 2 *`) earlier and harder
than §3's validation does: no times appear at all.

Local time is the right default for a local-first single-machine tool; the label
says so. (Cloud Scheduler recommends UTC specifically to dodge DST anomalies
across regions — not MUR's situation.)

**Non-goal:** `mur agent schedule`'s own scheduling UI is a different component
and out of scope.

## Files touched

- `mur-core/src/cmd/fleet/loop_run.rs` — `DonePolicy`, `done_policy`,
  `LoopStop::QueueDrained`, break site, `stop_reason_label` arm
- `mur-core/src/cmd/fleet/settings.rs` — write-boundary validator
- `mur-hub-gui/src-tauri/src/fleet.rs` — `cron_preview` Tauri command
- `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` — policy select, cron
  preset + preview
- `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts` + test — preset
  composition, revert the `marker:` check
- `mur-hub-gui/ui/src/i18n/{en,zh-TW}.ts`
- `CLAUDE.md` — the `done_when: marker:<TEXT>` sentence gains `queue-empty`

## Out of scope

- Injecting the marker contract into agent prompts (rejected in §2, with reason)
- A structured cron builder or a humanized cron description (§4)
- `mur agent schedule`'s scheduling UI
- Any change to how `budget_usd`, `deadline`, or stuck-detection *behave* —
  this spec only stops them from being silently misconfigured
