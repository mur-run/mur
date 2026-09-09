# `/deep-research`: a slash command, a skill, and progress an agent can see

**Status**: designed, not started.
**Decisions taken in brainstorming**: (C) ship BOTH a built-in murmur slash
command AND keep/upgrade the `mur-deep-research` skill; ALSO expose run
progress to agents through `fleet_run`, not only to humans.

## Problem

Deep research exists and works — `mur deep-research "question"` runs a
guarded fleet loop, writes a progress file, and the bare command renders a
panel. But three surfaces cannot reach it well:

1. **murmur has no `/deep-research`.** From the TUI the user must drop to
   `!mur deep-research "…"`, which runs in the shell-card and blocks the
   turn with no panel, no progress, no kill-switch hint.
2. **The skill is prose only.** `mur-deep-research` (in
   `mur-core/src/skills/mur_deep_research.yaml`) tells the model *that* the
   command exists, but not the flag surface, nor how to read progress, nor
   that `fleet_run` is the sanctioned way for an agent to trigger it.
3. **An agent that calls `fleet_run` is blind for up to an hour.** The tool
   spawns `mur deep-research "<q>"`, waits (default 1800 s, max 3600 s), and
   returns the output tail. There is no run id to poll, so `mur_job_status`
   — whose own description says "a timeout means MUR stopped waiting, NOT
   that the work failed — ask here instead of re-dispatching" — has nothing
   to be asked about. Every timeout is a coin-flip re-dispatch.

## What already exists (the ground this stands on)

### CLI surface — `mur deep-research`

Parsed in `mur-core/src/cli/mod.rs:456-463` with
`args_conflicts_with_subcommands = true`, so a bare question and a
subcommand are mutually exclusive:

| Invocation | What it does | Source |
|---|---|---|
| `mur deep-research` | Read-only status panel: model, fleet, each worker's running/egress state, last-run progress | `cmd/deep_research/panel.rs` |
| `mur deep-research "<question>"` | Preflight (workers exist, egress granted, fleet exists) → safe auto-repair (start workers, re-pin gateway) → guarded loop | `cmd/deep_research/ask.rs` |
| `mur deep-research setup` | Interactive wizard: model, worker count, budget, egress consent, browser consent | `cmd/deep_research/setup.rs` |
| `mur deep-research provision` | Scripted worker creation, flags below | `cmd/deep_research/provision.rs` |
| `mur deep-research run <fleet>` | Thin wrapper over `mur fleet run --loop` with overrides | `cmd/deep_research/run.rs` |

`provision` flags (`mur-core/src/cli/actions.rs:770-815`):
`--count N`, `--prefix <p>` (workers become `<p>_1..N`), `--model <alias>`
(models.yaml alias; default `claude_haiku`), `--grant-egress` (BroadAudited,
prompts `[y/N]` per worker), `--grant-browser`, `--deny-host <h>`
(repeatable), `--yes`, `--render-engine agent-browser|obscura`.

`run` flags (`actions.rs:823-836`): `--max-iterations N`, `--deadline 30s|5m|2h`,
`--budget-usd F`. These override `fleet.yaml`'s `loop.*` for one run.

Kill-switch: `mur fleet stop deep-research` (writes `.stopped`).
Canonical fleet name: `DEFAULT_FLEET_NAME = "deep-research"`
(`cmd/deep_research/status.rs:11`).

### Progress — already written, just not read from the right places

`mur-core/src/cmd/fleet/progress.rs` is the single-source model:

```rust
pub const PROGRESS_FILE: &str = ".run_progress.json";   // ~/.mur/fleets/<name>/
pub const STALE_AFTER_SECS: u64 = 600;
pub struct RunProgress { schema_version, run_id, question, started_at,
    finished_at, outcome, iteration, model, budget_usd, spend_usd, steps }
pub struct StepProgress { id, worker, phase, desc, state, cost_usd, started_at, ended_at }
```

The loop (`cmd/fleet/loop_run.rs:415-435`) creates it with a fresh
`uuid::Uuid::now_v7()` run id, saves before the first iteration, on every
step observation (`:556-563`), after every iteration (`:673-680`), and once
more with `outcome` at exit (`:719-725`). Saves are atomic
(tmp + rename, `progress.rs:121-137`) and never fail the run.

`outcome` ∈ `converged | max-iterations | deadline | budget | stopped |
stuck | failed`. `iteration_summary_line` (`progress.rs:152`) is the one
human line: `iteration 3 done: 4✓ 0✗ 1 pending · spend $0.42/$5.00 · model claude_haiku`.

So: **the answer to "can the fleet report progress?" is yes, it already
does — to a file.** Nothing needs to be invented on the writer side. What is
missing is readers.

### Run status — has a hole exactly where fleet_run needs it

`mur_job_status` reads `mur_core::run_status::status_of(mur_home, run_id)`
(`mur-mcp-server/src/tools.rs:808-822`). The loop records
`RunKind::Fleet` per **iteration** under
`loop-<name>-<uuid>-<iter>` (`loop_run.rs:631-632`). The **top-level**
`RunProgress.run_id` is never recorded in the run store, and `fleet_run`
never returns any id. Hence the blindness.

### murmur slash commands — three lists to touch

Per the earlier `2026-09-08-murmur-slash-arg-completion-design.md`, a
command lives in three places that nothing ties together:
`parse_slash` (`cmd/agent/cli/app.rs:219-271`), `COMMANDS` in
`complete.rs`, and `HELP` in `cli/mod.rs:196`. That spec's
`help_lists_every_command_the_parser_accepts` guard exists; this command
must land in its `one_of_each()` list too.

## Design

Three deliverables, each small, sharing one primitive.

### D0 — shared primitive: `progress::load_view`

One pure reader every surface calls, so no two renderings disagree:

```rust
// mur-core/src/cmd/fleet/progress.rs
pub struct ProgressView { pub progress: RunProgress, pub age_secs: u64, pub stale: bool, pub live: bool }
pub fn load_view(mur_home: &Path, fleet: &str) -> Option<ProgressView>
```

`live = finished_at.is_none() && !stale`; `stale = finished_at.is_none() &&
age_secs > STALE_AFTER_SECS`. `render_progress` in `panel.rs` is refactored
to take a `&ProgressView` (behaviour unchanged). `load` (already public,
returns `(RunProgress, u64)`) stays for compatibility.

### D1 — built-in `/deep-research` in murmur

Grammar (mirrors the CLI so nothing new to learn):

```
/deep-research                      status panel (same text as bare CLI)
/deep-research <question…>          run; question is the rest of the line
/deep-research status               alias of bare, for people who type it
/deep-research stop                 kill-switch → mur fleet stop deep-research
/deep-research setup                prints "run `mur deep-research setup` in a
                                    terminal — it asks for egress consent"
/research …                         alias
```

`SlashCmd::DeepResearch(Vec<String>)` — same shape as `Panel`/`Skill`,
sub-dispatch inside the handler. Words `status|stop|setup` are reserved
only as the **sole** first word; `"status of the Rust 2027 edition"` is a
question.

**Run semantics.** The handler does NOT call the model. It runs the same
code path as `mur deep-research "<q>"` (`ask::cmd_ask`) in a background
task, surfaces the panel as a card, and streams `iteration_summary_line`
into the chat as system lines while `load_view().live`. The turn is not
blocked — the user can keep chatting. On exit the card shows `outcome` and
the report location (whatever `cmd_ask` prints last). `/deep-research
stop` writes the kill-switch; the loop's own guard turns it into
`outcome = stopped`.

**Why not hand the question to the agent and let it call `fleet_run`?**
Because the murmur-side agent is not necessarily on the `fleet_run.agents`
allowlist, and a slash command that silently fails on a config gate is
worse than none. The slash command is the *human's* door — it uses the
human's privileges (a `mur` subprocess, exactly like `!mur …`), not the
agent's.

**Preflight failure** (no workers / no egress / no fleet) surfaces
`plan_preflight`'s `bail!` text verbatim as the card body — those messages
already say "run `mur deep-research setup`".

**Completion menu** (`complete.rs`): row `("deep-research", "run the
research fleet, or show its status", &["status", "stop", "setup"])`. Per the
09-08 spec, an argument provider for the free-text question is
deliberately *not* offered.

### D2 — skill upgrade: `mur-deep-research` v0.2.0

Same file, `mur-core/src/skills/mur_deep_research.yaml`. Keep the
trigger/abstract; replace `context` with a table of the flag surface above,
plus two new sections:

- **"If you are an agent, not a human"**: never shell out to
  `mur deep-research` — use the `fleet_run` tool with
  `fleet: "deep-research"`, `goal: "<question>"`. Explain the deny-by-default
  gate (`fleet_run.agents` / `fleet_run.fleets` in `~/.mur/config.yaml`)
  and that a refusal is a config fact to report, not a thing to work around.
- **"Reading progress"**: the file path, the `outcome` vocabulary, staleness
  = 600 s, and — once D3 lands — "use `mur_job_status` with the `run_id`
  `fleet_run` returned".

Version bump so the built-in-skill re-seed logic replaces the stale copy.

### D3 — `fleet_run` reports progress to the calling agent

Two additions to the tool, both backward compatible.

**(a) Register the run, return the id.** `fleet_run` cannot know the loop's
uuid before spawning, so the loop learns it from the environment instead:

```
MUR_RUN_ID=<uuid>     # if set, loop_run uses it as RunProgress.run_id
```

`fleet_run` mints `uuid::Uuid::now_v7()`, passes it in the child env,
records a `RunKind::Fleet` run under that id *before* spawn (state
`running`, heartbeat = now), and the loop's existing per-iteration and exit
writes stamp heartbeat and terminal state on it. `mur_job_status <id>` now
answers `running · alive` mid-run and `done · converged` after. The
loop's exit also writes to `run_status`, so a timed-out `fleet_run` and a
finished loop reconcile the same way `status_of` already reconciles
cache vs channel (`run_status/mod.rs:229-260`).

**(b) A `wait: false` mode.** New optional input:

```json
"wait": { "type": "boolean", "description": "false → return immediately with run_id; poll with mur_job_status" }
```

Default `true` keeps today's behaviour byte-for-byte. With `false` the tool
returns:

```
run_id: 0192f…   fleet: deep-research   progress: ~/.mur/fleets/deep-research/.run_progress.json
poll: mur_job_status {"run_id":"0192f…"}    stop: mur fleet stop deep-research
```

The child is detached (`kill_on_drop(false)`, own process group) so the
tool returning does not kill the research. In `wait: true` mode the
returned tail is **prefixed** with the same `run_id:` line, so a caller
that hits the 3600 s ceiling still has the id in the timeout error:

> fleet run `0192f…` timed out after 3600s and was killed; … check
> `mur_job_status`.

**(c) Progress in `mur_job_status`.** When the record's `run_kind` is
`Fleet` and a `.run_progress.json` exists with the same `run_id`, append
`iteration_summary_line` and the running steps' `desc` to the status
text. Read-only, via D0's `load_view`. Nothing here parses the loop's
stdout.

### Sandbox note

`fleet_run`'s carve-ins are computed at seal time from the same allowlist
(`fleet_run.rs:5-9`). Setting an env var and detaching a child needs no new
grant; recording the run needs write access to `~/.mur/runs/` — add it to
the carve-in set beside `fleets/`, `commander/`, `conversations/`, gated the
same way.

## Out of scope

- A streaming/event channel from the loop to the agent. The file is the
  contract; polling it is enough at research timescales (minutes).
- Multiple concurrent deep-research runs. Today one progress file per fleet
  means one run at a time; `fleet_run` in `wait: false` mode must refuse
  with the existing run's id if `load_view().live`.
- Any change to the preflight/consent model. Egress consent stays human.

## Test plan

| Area | Test |
|---|---|
| parse | `/deep-research`, `/research what is X`, `/deep-research status`, `/deep-research status of X` (question), `/deep-research stop` |
| help/complete | `one_of_each()` gains `DeepResearch`; the existing structural test then covers `HELP` |
| progress | `load_view` stale/live/finished truth table with a synthetic file and injected age |
| loop | `MUR_RUN_ID` set → `RunProgress.run_id` equals it; unset → uuid v7 as today |
| fleet_run | `wait:false` returns within 1 s with a `run_id:` line; refuses when a live run exists; `wait:true` output starts with `run_id:` |
| job_status | Fleet run with matching progress file → status text contains `iteration N done:` |
| panel | `render_progress` output unchanged for the existing fixtures |

## Sequence

1. D0 (`load_view`, panel refactor) — no behaviour change, lands alone.
2. D3(a) `MUR_RUN_ID` + run registration — unlocks polling even before
   the tool changes.
3. D3(b)(c) `wait` + job_status enrichment.
4. D1 slash command.
5. D2 skill bump (last, so it describes what actually shipped).
