# Browser testing — record/replay an end-to-end test

Fixed workflow for a browser end-to-end test: check `browser status`, hand
off to the **auth** mode when a login profile is missing, record with
`--mode test`, replay with `--heal` and read the verdict, export once green
or yellow. Never edits an `assert` step and never retries a declined heal by
replaying the same recording again — a decline means re-record.

## Consumes

- **auth mode** (`references/auth.md`) — for the login profile, when the
  target site needs one.
- `mur browser replay --heal` (Task 4/5 offline heal) — self-heals broken
  locators inside the budget, D1–D4.
- `mur browser export` — turns a recorded run into a Playwright `.spec.ts`.

## The fixed workflow

1. `mur browser status` — read the `profiles:` and `runs:` lines.
2. If the target site needs a profile and it is missing or unhealthy, stop
   and hand off to **auth mode**. Do not attempt login yourself
   (auth mode rule 1: the agent never enters credentials). Come back once
   `mur browser status` shows the profile healthy.
3. Record:
   ```
   mur browser record --run <name> --mode test --trace --profile <site>
   ```
   `--profile` is only needed when the flow requires an authenticated
   session; omit it for a logged-out test.
4. Replay with healing on:
   ```
   mur browser replay <name> --heal
   ```
5. Export once the run is green or yellow:
   ```
   mur browser export <name> --out <path>
   ```

## Mode is always `test`

`--mode test` is not a default to leave alone — it is the contract that makes
this mode's replay behavior mean anything. `mode: test` is what makes
`mur browser replay --heal` hold healed steps to a budget
(`--max-heal-ratio`, default in `mur_browser::heal::DEFAULT_HEAL_RATIO`) and
turn the run **red** via `ReplayReport::budget_exceeded` when too many steps
needed healing (D4) — that budget check does not fire for `mode: automation`.
Never pass `--mode automation` from this mode.

Trace is on by default here (`--trace`): a test run should always carry a
trace for post-mortem, unlike a routine automation run where it is optional.

## Reading the replay verdict

`mur browser replay` prints one summary line and writes
`~/.mur/browser/runs/<name>/report.md`. The verdict (`ReplayReport::verdict`)
is one of:

| Verdict | Meaning | Your move |
|---|---|---|
| `green` | all steps passed, nothing healed | export |
| `yellow` | some steps healed, all healed steps were verified and stayed under budget | export — a healed step's locator was already rewritten in `actions.yaml` (D3) |
| `red` | a step failed outright, or `budget_exceeded` is set | **stop** — see below, do not export |

## A declined heal is not a retry-until-it-works loop

Self-heal can decline in two distinct ways. Both mean: report to the human,
do not retry.

1. **Rolled back (mid-run)** — the offline matcher proposed a locator, but
   the very next element step also missed on the snapshot the heal
   predicted, so the heal was speculative and got reverted. The step outcome
   message reads `"... — <note>, but the previous heal was rolled back"`.
   Re-running replay against the same stale recording will not fix this —
   the page changed enough that the recording itself is wrong.
2. **Over budget (D4, end of run)** — more element steps healed than
   `--max-heal-ratio` allows for this run's step count. `report.verdict()` is
   `"red"` and `report.budget_exceeded` carries the message: *"heal rate too
   high: N of M element steps healed (allowed K at --max-heal-ratio R); the
   recording is stale — re-record it"*. That is the literal next action:
   **re-record**, do not replay again and do not raise `--max-heal-ratio` to
   push it through.

In both cases: nothing was written back (`written_back` stays `0` — the CLI
only applies heals when `report.failed == 0 && report.budget_exceeded.is_none()`),
`actions.yaml` is untouched, and the correct next step is a fresh
`mur browser record` of the same flow, not a second `replay --heal`.

## The "never" list

- **Never edit `actions.yaml` by hand to change an `assert` step** — not the
  expected text, not the selector, not to make a red run pass. An assert
  failing means the flow under test broke or the page changed; fix the app or
  re-record, never the assertion.
- **Never delete a step to make a test green.** A step that keeps failing or
  keeps heal-declining is signal, not noise. Report it; do not prune it out of
  `actions.yaml`.
- **Never retry a declined heal by re-running `replay --heal` on the same
  recording.** Rolled-back and over-budget heals both mean the recording no
  longer matches the page — re-record instead.
- **Never pass `--mode automation` to `record`, and never edit a recorded
  run's `mode:` field after the fact** — it silently turns off the D4 budget
  check this mode relies on.

## Command reference

| Command | Purpose |
|---|---|
| `mur browser status` | List profiles and recorded runs |
| `mur browser record --run <name> --mode test --trace [--profile <site>]` | Record a new test run |
| `mur browser replay <name> --heal [--max-heal-ratio <0.0-1.0>]` | Replay headlessly with offline self-heal |
| `mur browser export <name> [--out <path>]` | Write the run as a Playwright `.spec.ts` (stdout if `--out` omitted) |
| `mur browser show <name>` | Inspect a recorded run's steps |

## Handing off

If the flow needs a profile you don't have, stop and name the exact command
for the human: `mur browser auth <site> --url <URL> --browser <engine>` (see
**auth mode**) — do not attempt it yourself.
