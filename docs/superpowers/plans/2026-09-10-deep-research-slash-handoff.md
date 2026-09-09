# Handoff — `/deep-research` slash command + agent-visible progress

> **For fleet `develop-rust`.** Execute with `mur-executing-plans`, task by task, in the order below. Delegate Rust to `rustsmith`, tests to `qa`, commits to `repomanager`; `pm` ticks the checkboxes in the plan file. Stop after each task for review.

**Plan:** `docs/superpowers/plans/2026-09-10-deep-research-slash-plan.md` (5 tasks, checkbox-tracked — tick there, not in memory)
**Spec:** `docs/superpowers/specs/2026-09-10-deep-research-slash-design.md`
**Repo:** `/Volumes/Firecuda4tb/Projects/mur`
**Base:** `main` @ `0b2e25b7` (spec + plan committed; no code touched)
**Branch:** create `feat/deep-research-slash` from `main` before Task 1
**Status:** nothing executed yet — zero of five tasks done

## Goal in one breath

A murmur `/deep-research` command, a `mur-deep-research` skill that documents the real flag surface, and a `fleet_run` tool that returns a `run_id` an agent can poll via `mur_job_status` — so a deep-research run is never a blind wait.

## Task order and crate ownership

| # | Task | Crates | Reviewer gate |
|---|---|---|---|
| 1 | `progress::load_view` + `render_progress(&ProgressView)` refactor | mur-core | 3 existing panel fixtures byte-identical |
| 2 | Loop honours `MUR_RUN_ID`, self-registers in `run_status`, heartbeats per iteration, maps 9 outcomes → state | mur-core | one test per outcome variant |
| 3 | `runs/` sandbox carve-in; `fleet_run` mints/returns `run_id`, `wait:false`; `mur_job_status` appends `progress:` | mur-agent-runtime, mur-mcp-server | default `fleet_run` output unchanged except leading `run_id:` line |
| 4 | `/deep-research` + `/research` in murmur — argv subprocess, 5 s ticker, non-blocking | mur-core | help/complete parity tests pass |
| 5 | Skill bump to 0.2.0 — written last, describes what shipped | mur-core | — |

Tasks 1 → 2 → 3 are sequential (each builds on the previous). Task 4 depends on 1 only; Task 5 on everything. Still: **one cargo build at a time — never fan out cargo**.

## Build / test env (non-negotiable)

```
PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
ORT_STRATEGY=download
MUR_WEB_DIST=$HOME/Projects/mur-web/dist
```

- Test: `cargo nextest run -p <crate> <filter>`
- Before every commit: `cargo fmt` then `cargo clippy -p <crate> -- -D warnings`
- Commit per task, conventional prefix (`feat(fleet):`, `feat(murmur):`, `feat(mcp):`, `docs(skill):`)

## Decisions already made — do not relitigate

1. **Child-side registration.** `mur-agent-runtime` has no `mur-core` dep. The loop registers itself in `run_status` using the `MUR_RUN_ID` it is handed; `fleet_run` only mints the id and passes it in env. Accept the sub-second "no run recorded" window — the `wait:false` output states it.
2. **Outcome vocabulary is 9, not 7.** `loop_run.rs:334` `outcome_label` has `converged`, `max-iterations`, `deadline`, `budget`, `queue-drained` → `Done`; `stopped`, `commander-killed` → `Stopped`; `awaiting-approval` → `Blocked`; `stuck` → `Failed`.
3. **Slash command runs with the human's privileges** — a `mur` subprocess like `!cmd`, never the agent's `fleet_run` tool.
4. **Progress file is the contract.** No new event channel. `~/.mur/fleets/deep-research/.run_progress.json` is read by all three surfaces through `load_view`.
5. **`progress::load()` returns `SystemTime`, not `u64`** — `load_view` converts to age-seconds. The spec had this wrong.

## Known traps (found at plan time, already accounted for in the plan)

- No `temp_env` dev-dep — Task 2 factors a pure `resolve_run_id_from(env_value: Option<&str>)` so the test needs no env mutation.
- tokio is 1.52 — `Command::process_group` is a direct method, no `tokio::process::CommandExt` gymnastics.
- `mur_core::cmd::fleet::progress` is already `pub` — no re-export needed.
- `fleet_run.rs:170-186` spawns with `.kill_on_drop(true)`; `wait:false` must **not** drop the child — detach it (Task 3 spells this out).
- Sandbox carve-in loop at `policy.rs:358` is `["fleets", "commander", "conversations", "artifacts"]`; add `"runs"` and extend the guard test at `policy.rs:1150-1178` — do not add a second loop.

## Anchors (so nobody re-explores)

| What | Where |
|---|---|
| progress reader | `mur-core/src/cmd/fleet/progress.rs:158` `load()` |
| panel renderer + fixtures | `mur-core/src/cmd/fleet/panel.rs:46`, fixtures `:213-258` |
| loop mints id / terminal write / per-iter save | `mur-core/src/cmd/fleet/loop_run.rs:420`, `:719-725`, `:673-680` |
| `RunState` / store save / update | `run_status/mod.rs:123-145`, `store.rs:25`, `store.rs:92` |
| reference run-record construction | `executor/dag.rs:1055-1067` |
| `fleet_run` spawn / timeout error | `mur-agent-runtime/src/tools/fleet_run.rs:170-186`, `:188-201` |
| sandbox carve-in + guard test | `policy.rs:358`, `:1150-1178` |
| `mur_job_status` renderer + tests | `mur-mcp-server/src/tools.rs:808-843`, tests `:1002-1052`, helper `call_tool_in :981` |
| `SlashCmd` / `parse_slash` / dispatch / `HELP` | `app.rs:158`, `:212-271`, `cli/mod.rs:2222-2271`, `:196` |
| help/complete parity tests | `cli/mod.rs:3130-3197` |
| completion table | `complete.rs:164-200` |
| `!cmd` shell path to mirror | `cli/mod.rs:1781` → `StreamMsg::ShellDone` → `app.push_shell` `app.rs:1467`; `app.push_system` `app.rs:925` |
| skill | `mur-core/src/skills/mur_deep_research.yaml` |

## Definition of done

- [ ] All five tasks ticked in the plan file, one commit each on `feat/deep-research-slash`
- [ ] `cargo nextest run -p mur-core -p mur-agent-runtime -p mur-mcp-server` green
- [ ] Manual: `mur fleet run deep-research --goal "x"` from a terminal shows `run_id:`; `mur_job_status <id>` answers with a `progress:` line mid-run
- [ ] Manual: `/deep-research status` in murmur renders the same numbers as `mur deep-research` panel
- [ ] PR opened against `main`, body links spec + plan, lists any corrections found during execution (add a "Corrections" section to this file, as `2026-09-07-murmur-secret-handoff.md` does)

## Report back

Per task: commit hash, test filter run + pass count, anything the plan got wrong. At the end: PR URL and the corrections list.
