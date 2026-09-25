# Browser automation — repeatable browser task

Fixed workflow for a routine, repeatable browser task (not a test): check
`browser status`, record with `--mode automation`, **always dry-run the
action plan for a human before the first real run**, then execute. Automation
carries no assertions — a failed step is retried up to twice and then
reported, never silently treated as a pass.

## Consumes

- **auth mode** (`references/auth.md`) — for the login profile, when the
  target site needs one.
- Task 2's domain allowlist (`mur_browser::guard::check`) — enforced by
  `mur browser replay` on every navigation; this mode never tries to route
  around it.
- `mur browser replay --dry-run` (Task 4) — checks the run against the
  profile allowlist without launching a browser, so the plan can be reviewed
  before anything executes.

## The fixed workflow

1. `mur browser status` — read the `profiles:` and `runs:` lines.
2. If the target site needs a profile and it is missing or unhealthy, stop
   and hand off to **auth mode**. Do not attempt login yourself.
3. Record:
   ```
   mur browser record --run <name> --mode automation --profile <site>
   ```
4. **Before the first real run, always dry-run it:**
   ```
   mur browser replay <name> --dry-run
   ```
   This checks every navigation against the profile's domain allowlist
   without launching a browser. Show the resulting action plan to the human
   and wait for their go-ahead — do not chain straight into a real run.
5. Only after that sign-off, run it for real:
   ```
   mur browser replay <name>
   ```

## Mode is always `automation`

`--mode automation` is what this mode is for — it skips the healed-step
budget check (`ReplayReport::budget_exceeded`, D4) that `mode: test` enforces,
because there is no fixed assertion set to hold a budget against. Never pass
`--mode test` from this mode; that mode belongs to **testing**.

## Parallel runs and `storageState`

When running more than one automation worker against the same profile:

- The saved `storageState` (the encrypted profile under
  `mur browser auth`) is loaded **read-only** by every worker. Nothing this
  mode does may write back to the profile's state file.
- Each worker gets its **own browser context** — never share a live browser
  context across workers, even against the same profile.
- Cookie or storage changes that happen during a run are runtime-only and are
  **never** written back to the saved profile. If the site rotates a session
  token mid-run, that is expected churn, not something to persist.

## No assertions, bounded retries

Automation mode has no pass/fail assertions the way testing mode does. A
step either completes or it does not:

- On a step failure, retry the run up to **2 times total**.
- After 2 retries, stop and report the failure with whatever detail
  `mur browser replay` printed (which step, what error). Do not guess at a
  fix and do not re-record.
- Never decide on your own that a run "should count as" successful because
  it looked close. There is no verdict to lean on here — report exactly what
  happened.

## Never list

- **Never** perform an irreversible action — payment, deletion, submission —
  unless the human has explicitly told you to, in this conversation, this
  turn. A step recorded earlier that happens to be irreversible does not
  carry standing permission to run it again unattended.
- **Never** attempt to navigate outside the profile's allowed domains.
  Task 2's guard (`mur_browser::guard::check`) enforces this in
  `mur browser replay` already — if a step gets rejected for being
  off-allowlist, that is the guard working as intended. Do not add
  `--allow-domain` to the profile, retry with a different profile, or
  otherwise try to get around it. Report the rejection instead.
- **Never** enter credentials yourself — that is auth mode's one rule,
  inherited here unchanged.
