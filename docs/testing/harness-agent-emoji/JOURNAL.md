# Harness Test Journal — mur agent operations + 7-agent dev team

> Append-only operation log + cross-session recovery doc.
> **If resuming in a new session, read "Recovery" first.**

## Mission

Harness-test `mur agent` operations; build a 7-role dev team of **real mur runtime agents**
that collaborate (A2A) to ship a small **`mur agent emoji`** feature. Record everything.
Batch bugs (3 at a time) → fix → PR (author `karajanchang`) → CI → auto-merge on green.

## Decisions (locked by user 2026-06-02)
1. Team = real mur runtime agents (`mur agent create` + `mur agent send`).
2. Deliverable = `mur agent emoji` (status→emoji; shown in `list` + agent card).
3. CLI scope = representative sweep (happy-path + key error).
4. PR auth = CI green → auto-merge; commit author `karajanchang`.
5. Agents run on **real Claude** via local OAuth bridge (ANTHROPIC_BASE_URL=http://127.0.0.1:8088, cc-proxy). User confirmed: ignore the sk-ant-oat key-format warning.

## Environment
- Repo: `/Volumes/Firecuda4tb/Projects/mur`; binary under test: `target/release/mur` (2.22.4).
- Isolated test home: `MUR_HOME=/tmp/mur-harness-home` (keeps real ~/.mur clean).
- Team agents: `tt-pm tt-arch tt-rust tt-devops tt-review tt-sec tt-qa` (prefix `tt-`). Do NOT touch real agents (agent_t, alice, Author, carol, tg-bridge, tgX).
- Each tt- agent: network.outbound=unrestricted, ANTHROPIC_API_KEY keychain secret, role system prompt.

## Recovery (read on resume)
1. `git branch --show-current` — work branches: `test/harness-agent-emoji` (this journal + artifacts), `fix/agent-robustness-batch1` (PR #332), `feat/agent-emoji` (emoji feature, in progress).
2. Restart the team: `bash docs/testing/harness-agent-emoji/team.sh start-all` (sources ~/.zshrc for bridge creds; runs under one waiting supervisor — launch via Bash run_in_background).
3. Re-run the relay if needed: `python3 docs/testing/harness-agent-emoji/relay.py`.
4. Check PR #332 CI: `gh pr checks 332`. Merge on green.

## Phase status
| Phase | Description | Status |
|-------|-------------|--------|
| 0 | Infra/journal/build | done |
| A | CLI representative sweep | DONE (3 bugs found + fixed) |
| C | Build 7-agent team + A2A | DONE (all 7 live on real Claude) |
| (relay) | PM→QA collaboration relay | DONE — transcript saved, 6/7 handoff lines |
| E | Bug-fix PR batch 1 (#332) | MERGED ✅ |
| D | Emoji feature (#333) | MERGED ✅ (rebased on main, CI green) |

**FINAL: both PRs merged to main 2026-06-02. #332=3 bug fixes, #333=emoji feature.**

## Bug buffer
- Batch 1 (FIXED → PR #332, author karajanchang):
  1. `mur agent list` aborted on one malformed profile (missing `schema`) — now skip+warn in `collect_agents()` (lifecycle.rs).
  2. agent runtime ignored `--help`/`--version` (started daemon) — now handled in `supervisor::entrypoint()` + `subcommand::has_flag()`.
  3. workflows ignored `MUR_HOME` (`dirs::home_dir().join(".mur")` bypassed `paths::mur_root`) — fixed in `workflow_yaml::default_store()` + workflow.rs.

### UX observations (not counted bugs)
- Inconsistent agent-name arg position: `secret <AGENT> <cmd>` vs `perm <cmd> <NAME>`.
- `mur stats` floods one WARN per corrupt pattern YAML (should summarize).
- Runtime previously had NO `--help` (fixed in batch 1).

### Agent-subcommand sweep — arg-order matrix (all functional; placement inconsistent)
- `<verb> <name>`: stats, logs, history, skill list, mcp list, schedule list
- `<name> <verb>`: secret <name> list, queue <name> list
- `<verb> show <name>`: perm show <name>
- no name (global): doctor
Real ergonomics wart (name position unpredictable) but a fix is a breaking CLI change needing
design sign-off → documented as recommendation, NOT auto-fixed. No new crash bug → no batch-2 PR.

### Sweep coverage
top-level: stats, doctor, verify, model list, skill list, notes list, workflow list, source list,
team, chat, agent list (+bug), unknown-cmd error.
agent: create, list, status, card, send (real A2A), prompt set, perm set-mode/show, secret set/list,
stats, logs, hooks, peers, skill list, mcp list, schedule list, history, queue list, doctor.

## Collaboration findings (relay)
- All 7 roles produced coherent, role-appropriate artifacts; design converged (PM PRD → Architect `AgentStatus::emoji()` → Rust code → DevOps CI/versioning → Reviewer caught wire-break risk → Security flagged ZWJ/RTL injection on ingest → QA test plan).
- 6/7 emitted the required `HANDOFF ->` line. **Rust Engineer dropped it** (produced code, omitted handoff). QA's final handoff truncated at the word cap.
- Latencies 12–17s/role (real Claude via bridge); total ~104s.
- Full transcript: `relay-transcript.md`.

## Operation log
- 2026-06-02 Phase 0 infra; CLI surface captured (25 top-level, 40+ agent subcmds).
- Found BUG#1 (agent list), BUG#2 (runtime --help), BUG#3 (MUR_HOME workflows).
- Created 7 tt- agents; wired real-Claude bridge (network unrestricted + keychain secret).
- Ran PM→QA relay (real A2A `send`); transcript saved.
- Implemented + validated 3 fixes; PR #332 opened (author karajanchang); CI running.
- NEXT: implement emoji feature on feat/agent-emoji, then PR.
