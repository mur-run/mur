# `mur browser doctor`: check record/replay's toolchain before an agent trips on it

**Status**: implemented on `feat/browser-doctor`. This spec was written
*after* the code, from the code, so a reviewer can judge the behaviour
against a stated intent rather than against itself.
**Precedent**: `mur deep-research doctor [--render]` (PR #1478).

## Problem

`mur browser record` / `replay` spawn `npx @playwright/mcp@0.0.82`
(`mur-browser/src/proxy.rs:363-364`, pin in `mur-browser/src/lib.rs:33`),
which in turn launches Playwright's bundled Chromium. Nothing checks that
chain up front:

- `mur browser status` lists saved profiles and recorded runs only. A
  machine with no `npx` or no Chromium looks healthy.
- The first sign of trouble is `replay` failing to spawn — mid-run, often
  inside an agent that cannot fix it.
- `--help` for `status`, `list`, `show`, `broker`, `auth` still says
  "implemented in a later slice" / "handoff implementation follows",
  though they work.

## Goals

1. A read-only command that says whether record/replay can run here, and
   prints the exact install command for anything missing.
2. An opt-in live check that proves the pinned package and its Chromium
   revision actually render a page.
3. Correct the stale `--help` text.

## Non-goals

- **No `mur browser setup`.** `npx -y` fetches the package on first use;
  the only thing that realistically goes missing is Chromium, and the
  doctor prints its install command. Installing is the user's call.
- No check of saved profiles or cookies — that is `status`'s job.
- No self-healing replay (`--heal` still errors; its help now says so).

## Surface

```
mur browser doctor          # L1: install check
mur browser doctor --live   # L1 + L2: headless render on a loopback page
```

Exit code 0 when every check passes, non-zero otherwise (so scripts and
agents can gate on it).

## L1 — install check (default)

Runs in `doctor::doctor`, pure over its inputs (writer, PATH, browsers
dir, command runner) so tests never exec anything real.

| Check | Pass | Fail output |
|---|---|---|
| `npx` resolvable on **the unmodified `PATH`** | `✓ npx at <path>` | `✗ npx not found on PATH` + `install Node.js (it ships npx): https://nodejs.org` |
| `node --version`, `npx --version` exit 0 (only if npx found) | `✓ node runs`, `✓ npx runs` | ``✗ `node --version` failed`` |
| A **completed** Chromium build in the Playwright browsers dir | `✓ <build> in <dir>` + note that `--live` confirms the revision | `✗ no completed Chromium build in <dir>` + `install with: npx -y @playwright/mcp@0.0.82 install-browser chromium` |

Decisions:

- **PATH is the raw `PATH`**, the one `playwright_command` spawns with, not
  an augmented one. A pass must mean replay will find `npx`.
- **"Completed" means the build folder contains `INSTALLATION_COMPLETE`.**
  An interrupted download leaves the folder without it and must not pass.
- `chromium_headless_shell-*` is preferred over `chromium-*` (replay runs
  headless); among builds, the **numerically** newest revision is reported
  (`1217 > 1000 > 999`, not lexical).
- L1 cannot know which revision the pinned package wants, so it says so
  and points at `--live` rather than guessing.
- The install hint goes through the **pinned package's** own
  `install-browser` alias, so it fetches the revision that package
  launches — not whatever `npx playwright` resolves to today.

Browsers dir resolution (`doctor::browsers_dir`):

| Condition | Dir |
|---|---|
| `PLAYWRIGHT_BROWSERS_PATH` set, non-empty, not `0` | that path |
| `PLAYWRIGHT_BROWSERS_PATH=0` | *unknown* — browsers live in `node_modules`; print `?` and recommend `--live`, not a failure |
| macOS | `~/Library/Caches/ms-playwright` |
| Windows | `%LOCALAPPDATA%\ms-playwright` |
| other | `$XDG_CACHE_HOME/ms-playwright`, else `~/.cache/ms-playwright` |

## L2 — `--live`

1. Serve the same loopback JS-only page deep-research uses
   (`serve_render_page`, `RENDER_PAGE` → script writes `MUR-RENDER-42`).
2. Spawn `playwright_command` with **replay's flags**:
   `--headless --isolated --browser=chromium` (cf.
   `mur-browser/src/replay.rs:417-419`), so it tests replay's real path —
   bundled Chromium, not branded Chrome.
3. Over MCP stdio: `browser_navigate` → `browser_snapshot`; pass iff
   `render_passed(snapshot)` (the script's output, not the raw source,
   appears).
4. Whole exchange bounded by **90 s** (cold `npx -y` may download the
   package and start Chromium for the first time), then the child is killed.

Outcomes:

| Result | Output |
|---|---|
| Rendered | `✓ rendered in N.Ns` |
| Loaded, script didn't run | `✗ page loaded but its script did not run` |
| MCP/tool error | `✗ <error>`; if the page server was never hit, also the Chromium install hint |
| Timeout | `✗ no answer within 90s` |

A tool result with `isError: true` is an error; no snapshot is taken after
a failed navigate. The navigate/snapshot logic (`render_probe`) is
generic over `ToolCaller` so it is tested with a fake.

Without `--live` the command ends with
`(install check only — add --live to launch a headless browser)`.

## `--help` corrections

| Subcommand | New text |
|---|---|
| `status` | List saved profiles and recorded runs (does not check the toolchain; see `doctor`). |
| `list` / `show` | drop "(implemented in a later slice)" |
| `broker` | Run the secret broker (needs `MUR_BROWSER_BROKER_TOKEN`; `record` starts its own). |
| `auth` | Log in once in a headed browser and save the session as an encrypted profile. |
| `replay --heal` | Self-heal missed locators (not implemented yet; the flag errors). |

## Tests

Unit (`cmd/browser/doctor.rs`):

- missing npx → fails, points at nodejs.org, runs nothing
- healthy → passes, runs exactly `node --version`, `npx --version`
- failing `node` → fails with the probe named
- interrupted download → fails, prints the pinned install hint
- revisions sort numerically; `ffmpeg-*` ignored
- `PLAYWRIGHT_BROWSERS_PATH` override and `=0` → unknown; macOS default
- live flags include `--browser=chromium` and `--headless`
- probe passes only on rendered output; raw `RENDER_PAGE` fails
- failed navigate surfaces the error and skips the snapshot

CLI (`cli/mod.rs`): `doctor` parses with and without `--live`.

Manual: `mur browser doctor` and `mur browser doctor --live` on a real
machine (macOS arm64) before merge. The live path spawns `npx` and
Chromium, so it is not in CI.
