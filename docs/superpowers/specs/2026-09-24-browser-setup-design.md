# `mur browser setup`: install replay's Chromium, with consent, then prove it renders

**Status**: proposed. No code yet.
**Reverses**: the "No `mur browser setup`" non-goal in
`2026-09-24-browser-doctor-design.md` (see [Why reverse it](#why-reverse-it)).
**Precedent**: `mur deep-research setup` → `ensure_render_browser`
(`mur-core/src/cmd/deep_research/browser.rs:682`).

## Problem

`mur browser doctor` tells you what is missing and prints the fix:

```
  ✗ no completed Chromium build in ~/Library/Caches/ms-playwright
  install with: npx -y @playwright/mcp@0.0.82 install-browser chromium
```

The user then has to copy that command, run it, and re-run
`doctor --live` to learn whether it worked. Three steps, one of which is
retyping a pinned package version by hand. Deep research already does the
whole loop in one consented step; record/replay should too.

## Why reverse it

The doctor spec argued that installing is "the user's call". That still
holds. What changes is only *who types the command*: setup prints the exact
command, asks, and runs it only on a literal `yes`. Nothing is installed
that the user did not see and approve. The argument against "a second place
that downloads things" is answered by sharing the consent code with deep
research rather than writing a new prompt (see [Consent](#consent)).

## Goals

1. One command that takes a machine from "doctor fails on Chromium" to
   "`--live` passes".
2. The exact install command is printed **before** anything runs, and runs
   only on a literal `yes`.
3. End with the same live render `doctor --live` does, so "setup finished"
   means replay works.

## Non-goals

- **No Node.js / npx install.** If `npx` is missing, setup stops with the
  same nodejs.org hint doctor prints. Installing a language runtime is out
  of MUR's lane.
- **No `--yes` in v1** (decided 2026-09-24). Nothing downloads without a
  human typing `yes`, with no exception. Scripts and agents run the printed
  `npx … install-browser chromium` directly. `mur fleet install-deps --yes`
  is not followed as precedent here.
- No branded Chrome / Firefox / Edge. Replay runs `--browser=chromium`
  headless (`mur-browser/src/replay.rs:417-419`); that is the only build
  setup installs.
- No change to replay, record, or the pinned package version.

## Surface

```
mur browser setup
```

Interactive only. Exit 0 iff the final live render passes.

## Flow

```
1. L1 check          (doctor::doctor, same output)
2. npx missing?      → stop, exit 1 (nodejs.org hint already printed)
3. Chromium present? → skip to 6
   dir unknown (PLAYWRIGHT_BROWSERS_PATH=0)? → skip to 6
4. Print plan, ask   → anything but `yes`: "skipped", exit 1
5. Run install       → non-zero exit: ✗ + exit 1
                       re-check L1 Chromium; still missing: ✗ + exit 1
6. Live render       (doctor::live_check, same output) → its result is the exit code
```

Example, Chromium missing:

```
Playwright MCP (@playwright/mcp@0.0.82):
  ✓ npx at /Users/david/.local/bin/npx
  ✓ node runs (v22.22.0)
  ✓ npx runs (10.9.2)
Chromium for replay (headless):
  ✗ no completed Chromium build in /Users/david/Library/Caches/ms-playwright

Installing Chromium for replay does:
    run       npx -y @playwright/mcp@0.0.82 install-browser chromium
    into      /Users/david/Library/Caches/ms-playwright
    (downloads the Chromium revision this pinned package launches)
Type 'yes' to do this now (anything else = skip): yes
  … npx output, inherited …
  ✓ chromium_headless_shell-1246 in /Users/david/Library/Caches/ms-playwright
Live test (headless Chromium via Playwright MCP, local JS-only page):
  ✓ rendered in 6.1s
mur browser is ready.
```

Already healthy: steps 1 → 6 only, no prompt. Setup is safe to re-run.

### Decisions

- **Same command as doctor's hint.** The install argv comes from
  `doctor::install_hint()`'s source, not a second string, so the printed
  hint and what setup runs can never drift. Refactor: `install_argv() ->
  Vec<String>`; `install_hint()` becomes `install_argv().join(" ")`.
- **Same `PATH` as replay.** The install child is spawned on the
  unmodified `PATH`, like `playwright_command`, so it uses the same `npx`
  doctor reported.
- **Inherited stdio for the install** (like deep-research's
  `system_runner`) so the download's progress is visible. The live test
  keeps its captured MCP stdio.
- **`PLAYWRIGHT_BROWSERS_PATH=0` never installs.** Browsers then live in
  some `node_modules` doctor cannot see; setup says so and goes straight to
  the live test, which is the real answer.
- **A declined install exits 1**, unlike deep-research, where declining
  must not fail the wider wizard. Here the install *is* the whole command;
  exiting 0 after "skipped" would tell a script it is ready when it is not.
- **Doctor stays read-only.** Its Chromium hint gains one line:
  `or run: mur browser setup`.

## Consent

Deep research's prompt is inline in `ensure_render_browser`
(`deep_research/browser.rs:694-707`):

```rust
write!(output, "Type 'yes' to do this now (anything else = skip): ")?;
...
if line.trim() != "yes" {
```

Its install plans (Lightpanda / agent-browser) do not fit Playwright's
Chromium, so the function itself is not reusable. What is shared is the
rule. Extract it once:

```rust
/// Literal `yes` only; `y`, `Y`, `YES ` → false. Nothing runs on false.
pub fn literal_yes(input: &mut dyn BufRead, output: &mut dyn Write) -> Result<bool>
```

into `mur-core/src/cmd/consent.rs`, and call it from both
`ensure_render_browser` and browser setup. Deep research's existing tests
(e.g. `'y' is not consent; nothing may run`, `:828`) must pass unchanged.

This deliberately does **not** use `deps/install.rs::confirm`
(`[y/N]`, accepts `y`): a download the user did not type out in full is the
thing this rule exists to prevent.

## Non-interactive use

stdin not a terminal → bail before any check, same shape as
`deep-research setup` (`setup.rs:167`):

```
`setup` is interactive; in scripts run:
  npx -y @playwright/mcp@0.0.82 install-browser chromium
  mur browser doctor --live
```

An agent that calls `mur browser setup` gets the two commands it needs and
nothing is downloaded behind the user's back.

## Code shape

- `mur-core/src/cmd/browser/setup.rs` (new): `setup(input, output, path_var,
  browsers, probe, install, live) -> Result<()>`, pure over its inputs like
  `doctor::doctor`, so tests exec nothing.
- `doctor::doctor` returns an `L1Report { npx_ok, chromium: Found | Missing
  | Unknown }` alongside its output, instead of only `Ok`/`bail!`, so setup
  can branch without re-parsing text. `mur browser doctor`'s output and exit
  codes stay byte-identical.
- `cli/actions.rs`: `BrowserAction::Setup`; `dispatch.rs` wires the real
  probe, installer and `live_check`.
- FreeBSD audit: no new `cfg!`/`target_os` expected; if any appears,
  `scripts/check-freebsd-audit.py` will demand rows (lesson from #1487).

## Tests

Unit (`cmd/browser/setup.rs`):

- non-TTY → bails with both commands, runs nothing
- npx missing → exit 1, no prompt, installer never called
- Chromium present → no prompt, installer never called, live called once
- `PLAYWRIGHT_BROWSERS_PATH=0` → no prompt, live called
- missing + `yes` → installer called with exactly `install_argv()`, then
  live
- missing + `y` / `Y` / empty / EOF → "skipped", exit 1, installer and live
  never called
- plan printed **before** the prompt, and contains the full argv and dir
- installer exits non-zero → ✗, exit 1, live not called
- installer succeeds but no completed build appears → ✗ names the dir,
  live not called
- live fails → exit 1 (setup's success means replay works)

Shared: `literal_yes` table test; deep-research consent tests unchanged.
Doctor: existing 11 tests unchanged; one asserting the new
`or run: mur browser setup` line; `install_hint() ==
install_argv().join(" ")`.

CLI: `mur browser setup` parses.

Manual before merge (not in CI, it downloads Chromium): on macOS arm64,
with `PLAYWRIGHT_BROWSERS_PATH` pointed at an empty temp dir:
decline → exit 1, nothing downloaded; accept → build appears, live ✓,
exit 0; re-run → no prompt, exit 0.

## Open questions

1. **Offer setup from `mur browser replay` when it fails to spawn?**
   Out of scope here; worth a follow-up once setup exists.
