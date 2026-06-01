# Harness Test Journal — `mur agent export` (export-and-run round trip)

> Append-only operation log + cross-session recovery doc.
> **If resuming in a new session, read "Recovery" first.**

## Mission

Harness-test `mur agent export`: really **build agents, export them, and run the
exported artifact** — both headless (`mur-agent-runtime --load X.muragent`) and via
the **MuR Hub GUI** loading the `.muragent`. Build a small (2-3 role) real-agent team
that collaborates over A2A and observe handoff. Record every command + result.
Batch bugs (3 at a time) → fix → PR (author `karajanchang`) → CI → merge on green.

## Decisions (locked by user 2026-06-02)
1. **Export target** = `.muragent` + headless `mur-agent-runtime --load` **AND** build
   the MuR Hub Tauri GUI and load the `.muragent` through it.
2. **Agent scope** = lean 2-3 role team focused on the export round-trip + A2A handoff
   (not the full 7-role team).
3. **Stale `--help`** (advertises removed `gui`/`bin`/`.app` + gui-only flags) = counts
   as a bug; fix it this run inside the 3-bug batch → PR.
4. Error protocol: batch 3 bugs → fix → PR → CI → auto-merge on green; commit author
   `karajanchang <alan@twdd.com.tw>` (repo local config is `github-actions[bot]` — must
   `--author` override on hand commits; see [[project_cost_router_orchestrator]]).
5. Recovery: continue next session if token runs out.

## Key finding (drives the whole test)
`mur agent export --help` (binary 2.22.4) advertises 4 formats — `muragent`, `pkg`,
`bin`, `gui` (→`.app`) — plus gui-only flags `--theme/--icon/--clone-identity/--skip-notarize`.
**But the code (`cmd/agent/export.rs`) `bail!`s on `bin` and `gui`** with
"--format=… is no longer supported". Real runnable formats today: `muragent` (default,
signed v2) and `pkg` (legacy). The new run path is `mur-agent-runtime --load X.muragent`
or the MuR Hub app. → **Bug #1: stale `--help`.**

## Environment
- Repo: `/Volumes/Firecuda4tb/Projects/mur`; binaries: `target/release/{mur,mur-agent-runtime}` (2.22.4).
- Isolated test home: `MUR_HOME=/tmp/mur-export-test` (keeps real ~/.mur clean).
- Export agents prefixed `xx-`. Do NOT touch real agents or earlier `tt-`/`agent_t`/alice/etc.
- main @ `94d77105` (emoji feature merged).

## Recovery (read on resume)
1. `git branch --show-current` — branches: `test/harness-agent-export` (this journal +
   artifacts), `fix/agent-export-batch1` (bug-fix PR, off main).
2. Re-create agents: `bash docs/testing/harness-agent-export/setup.sh` (TBD).
3. Headless run check: `MUR_HOME=/tmp/mur-export-test target/release/mur-agent-runtime --load /tmp/mur-export-test/<name>.muragent`.
4. Check fix PR CI: `gh pr checks <N>`. Merge on green.

## Phase status
| Phase | Description | Status |
|-------|-------------|--------|
| 0 | Journal / branches / isolated home | in progress |
| A | export CLI sweep + find 3 bugs | pending |
| B | build 2-3 agent team + A2A handoff | pending |
| C | export → headless `--load` run round trip | pending |
| D | MuR Hub GUI build + load `.muragent` | pending |
| E | bug-fix batch 1 → PR → CI → merge | pending |

## Bug buffer (batch 1, target 3)
1. **Stale `--help`** — `mur agent export --help` advertises `bin`/`gui`/`.app` +
   `--theme/--icon/--clone-identity/--skip-notarize`, all of which `bail!` "no longer
   supported". Help must match the 2 real formats. (FOUND — pending fix)
2. _TBD_
3. _TBD_

## Operation log
- 2026-06-02 Phase 0: branched `test/harness-agent-export` off the emoji test branch;
  isolated `MUR_HOME=/tmp/mur-export-test`; created scratch agent `expbot`; confirmed
  format matrix: `muragent`✅ `pkg`✅ `bin`❌bail `gui`❌bail; confirmed
  `mur-agent-runtime --load <PATH>` exists. Logged Bug #1 (stale `--help`).
