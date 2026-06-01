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
| 0 | Journal / branches / isolated home | done |
| A | export CLI sweep + find 3 bugs | DONE (3 bugs) |
| B | build 2-3 agent team + A2A handoff | pending |
| C | export → headless `--load` run round trip | DONE (verified) |
| D | MuR Hub GUI build + load `.muragent` | pending |
| E | bug-fix batch 1 → PR → CI → merge | PR #334 OPEN (CI running) |

## Bug buffer (batch 1, target 3) — ALL FOUND
1. **Stale `--help`** — `mur agent export --help` (cmd `about` + `--format` help) advertises
   `bin`/`gui`/`.app` formats that `bail!` "no longer supported". Error msg & runtime are
   honest ("use muragent or pkg") but help lies. Fix: scrub help to the 2 real formats.
2. **Dead gui-only flags silently accepted** — `--theme/--icon/--clone-identity/--skip-notarize`
   only applied to the removed `gui` export, but the clap args linger. With `--format muragent`
   they are parsed and silently ignored — even `--icon /nonexistent.png` is accepted with exit 0.
   Fix: remove the dead flags from the export arg struct.
3. **`--load` clobbers an existing agent's identity keypair (DATA LOSS)** — loading a
   template-mode `.muragent` into a home that already has a same-named agent overwrites it with
   the sanitized (keyless) copy, deleting `identity.key`/`identity.pub`. After a self/re-load,
   `mur agent export <name>` fails "identity files not found". Clean repro in `/tmp/mur-bug3`.
   Fix: on install, preserve an existing agent's identity when the incoming package is
   template-mode (or refuse to overwrite without an explicit flag).

### Other findings (not in batch — documented)
- `mur-agent-runtime --load <x.murpkg>` fails with confusing "manifest YAML parse error:
  missing field `exporter`" — legacy `pkg` exports have no run path (only `.muragent` loads).
  Error should say "legacy .murpkg not loadable; re-export as .muragent". (low-risk, deferred)
- Export edge cases graceful: missing agent / missing --out / bad --format / missing out-dir /
  out=dir all error cleanly with exit 1. Overwrite of existing export is silent (acceptable).

## Operation log
- 2026-06-02 Phase 0: branched `test/harness-agent-export` off the emoji test branch;
  isolated `MUR_HOME=/tmp/mur-export-test`; created scratch agent `expbot`; confirmed
  format matrix: `muragent`✅ `pkg`✅ `bin`❌bail `gui`❌bail; confirmed
  `mur-agent-runtime --load <PATH>` exists. Logged Bug #1 (stale `--help`).
- 2026-06-02 Phase A: edge-case sweep (E1-E16). Found Bug #2 (dead gui-only flags silently
  accepted under muragent) and Bug #3 (identity clobber on `--load` into existing home,
  clean repro in `/tmp/mur-bug3`). Located fix sites: `cli/agent.rs` Export variant,
  `dispatch.rs:1161`, `mur-common/.../installer.rs::clear_except_data`.
- 2026-06-02 Phase C: headless round trip verified — `--load expbot.muragent` brings up the
  A2A supervisor ("agent ready", `agent.sock` created, graceful SIGTERM shutdown). Recipient
  round trip into a FRESH home installs profile + trust and `mur agent list` then shows it.
- 2026-06-02 Phase E: implemented 3 fixes on `fix/agent-export-batch1` (off origin/main):
  scrub help to 2 real formats, drop dead gui-only flags, preserve identity keypair in
  `clear_except_data` (+ regression test). Verified: fmt+clippy clean, 4 installer tests ok,
  end-to-end identity now survives `--load` and re-export exits 0. Commit `b7a74c97`
  (author karajanchang). **PR #334 opened; CI running.**
