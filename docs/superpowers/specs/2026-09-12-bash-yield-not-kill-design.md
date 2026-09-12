# bash: a timeout is a yield, not a kill

**Status:** Approved in conversation 2026-09-12 (defaults taken on all three open questions); awaiting plan.
**Scope:** `mur-agent-runtime` only (`tools/bash.rs`, new `tools/bash_jobs.rs`, `tools/registry.rs`, one fingerprint tweak in `task_runner.rs`). No protocol change on the wire. No new config keys beyond one optional cap.
**Parent:** `docs/superpowers/specs/2026-09-12-execution-limits-design.md` — this applies its D6 ("long-running tools return a handle, never block") to the last built-in tool that still blocks and kills.

## 1. Problem

A worker agent (rustsmith) was running Task 3's verification: `cargo test`
chained with `cargo fmt` and `cargo clippy` in one `bash` call. The call
hit `timeout_secs: 600`, the runtime **killed the process**, the model
retried with a smaller command, that one hit the 30 s default, and the turn
ended with `state failed / no output / exit code 1`. The code edits were
fine; the test run was killed mid-way twice and its result never existed.

The cause is one constant:

```rust
// mur-agent-runtime/src/tools/bash.rs:24
const MAX_TIMEOUT_SECS: u64 = 600;
```

but the bug is the semantics, not the number. Today `timeout_secs` means
"kill the child at T and report failure". A build that legitimately takes
11 minutes cannot be run at all, the model cannot know in advance which
commands will take 11 minutes, and when it guesses wrong the work is lost —
the child is killed, its partial output discarded, and the model is told
`command timed out`, which reads as *the command failed*.

Raising 600 to 3600 is the alternative the parent spec explicitly rejects
(D6, "the next tool that takes 20 minutes hits it again"). It also collides
with the stuck detector: unattended runs stop after N minutes without a
progress signal, and a single blocking `bash` call produces no signal for
its whole duration.

### Sibling hard limits (scan, 2026-09-12)

Requested by the user: "others may have the same hard limit". Found via
`mur project search` + grep over every runtime crate:

| Where | Limit | Verdict |
|---|---|---|
| `mur-agent-runtime/src/tools/bash.rs:17,24` | default 30 s, max 600 s, kills | **this spec** |
| `mur-agent-runtime/src/tools/fleet_run.rs` | was 1800/3600 s | fixed on main by #1279 — returns `run_id`, `timeout_secs` ignored since 2.80 |
| `mur-agent-runtime/src/tools/mcp.rs:17` | 120 s default, per-server `timeout_secs`, no cap | conforms to D6 ("a tool that needs longer must return a handle"); untouched |
| `mur-core/src/a2a_dial.rs:43` | 600 s / 90 s idle | fixed by #1279 heartbeats (D7); the beat is a separate tokio task and keeps ticking while a tool blocks |
| `mur-core/src/cmd/agent/cli/stream.rs:103` | murmur `!cmd` 30 s, kills | same disease, user-facing; **follow-up issue**, not this spec |
| `mur-agent-runtime/src/llm/{ollama,openai}.rs` | 60 s whole-request timeout, applies to streamed bodies too | a slow local model producing a long answer is cut off; should be an idle (between-chunks) timeout; **follow-up issue** |
| `mur-common/src/agent.rs` `hitl.timeout_secs` 300 | approval wait | already defers instead of timing out unattended (2026-08-19 spec); untouched |

## 2. Decisions

| # | Decision | Rejected alternative |
|---|---|---|
| D1 | **A timeout is a yield.** When `timeout_secs` elapses and the child is still running, `bash` returns a handle and the child keeps running. Nothing is killed by the clock. | Kill at T and add `background: true` (Claude Code's model) — the model must guess in advance which commands are slow; a wrong guess still loses the work. That guess is exactly what failed here. |
| D2 | **No ceiling on work, a ceiling on waiting.** `timeout_secs` keeps its name and its 600 s cap, but the cap now bounds only how long one tool call holds the turn. A job may run for hours across many `bash_wait` calls. | Make `MAX_TIMEOUT_SECS` configurable — moves the number into YAML and keeps the kill semantics; the parent spec's "don't just raise it". |
| D3 | **Jobs outlive the turn.** A running job survives the end of the model's turn so the agent can say "build is running, I'll check" and answer a later message with `bash_wait`. Jobs die with the runtime process (`kill_on_drop` stays) and with an unattended deadline. | Kill all jobs at turn end — forces the model to babysit every long command inside one turn, which is the blocking model with extra steps. |
| D4 | **A wait that returned new output is progress.** The stuck clock fingerprints tool calls by arguments; `bash_wait {job_id}` repeated would look stuck. The runtime folds the byte offset the wait reached into that fingerprint, so a wait that delivered bytes differs from the previous one and a wait that delivered nothing repeats. | Ask the model to pass a `since` cursor — pushes bookkeeping onto the model and it can pass the same value forever. |
| D5 | **Running is a status, not an error.** `ToolStatus::Running { job_id, bytes_seen }` joins `Ok / Failed / Denied`. A yielded call is `is_error: false`. The text says *still running*, never *timed out*. | Return `Err(ToolError::Execution("timed out"))` with the job id in the message — the structural status is what the model and the settlement card read; text markers were already removed once (`ToolStatus` doc comment). |
| D6 | **Two new tools, no new arguments to learn.** `bash_wait { job_id, wait_secs }` and `bash_kill { job_id }`. `bash` gains nothing except the changed meaning of `timeout_secs`; `timeout_secs: 0` means "return the handle at once" (background). `wait_secs` is accepted as an alias for `timeout_secs` on both tools. | One `bash` tool with `mode: start|wait|kill` — three behaviours behind one schema is harder for the model to read than three names. |
| D7 | **Output spools to disk, the model sees the tail.** Combined stdout+stderr streams to `<agent_home>/jobs/<job_id>.log`; each reply carries the bytes since the last reply, capped, plus the file path so the model can `read_file` any range. | Keep everything in memory — a 200 MB build log inside the runtime process. |

## 3. The model

### 3.1 Tool surface

```jsonc
// bash — unchanged shape, changed meaning of timeout_secs
{ "command": "cargo test -p mur-core", "cwd": "...", "timeout_secs": 120 }
// → Ok        { text: "<output>\n[exit code: 0]" }                 // finished in time
// → Running   { text: "<output so far>\n[still running after 120s — job_id: j-01J…; call bash_wait to keep waiting, bash_kill to stop; full log: ~/.mur/agents/<name>/jobs/j-01J….log]" }

// bash_wait — keep waiting on a yielded job
{ "job_id": "j-01J…", "wait_secs": 300 }
// → Ok / Failed { text: "<new output since last reply>\n[exit code: N]" }   // job finished
// → Running     { text: "<new output>\n[still running, 7m20s elapsed — …]" } // still going
// → InvalidInput                                                             // unknown job_id

// bash_kill — stop a job
{ "job_id": "j-01J…" }
// → Ok { text: "killed j-01J… after 9m12s (SIGKILL); last output:\n…" }
```

Defaults: `bash.timeout_secs` 30 (unchanged), `bash_wait.wait_secs` 60,
both clamped to `[0, 600]`. The 600 s cap is the **wait** cap; the tool
description says so in one sentence and never uses the word "killed" for
the clock.

`timeout_secs: 0` on `bash` spawns and returns `Running` immediately —
the background case, with no separate flag.

### 3.2 Job lifecycle

```
bash(cmd) ──spawn──▶ Job { id, child, spool, tail, started_at, exit: watch<Option<ExitStatus>> }
                │
                ├─ finished within timeout ──▶ Ok/Failed, job removed from table
                │
                └─ still running ──▶ Running{job_id}; job stays in table
                                          │
                     bash_wait ◀──────────┤  (any later turn)
                     bash_kill ◀──────────┤
                                          │
                     finished ──▶ result held in table until one bash_wait
                                  collects it, then removed (or reaped after
                                  JOB_RESULT_TTL, default 1 h, so an
                                  abandoned handle does not leak)
```

One tokio task per job owns the child: it copies stdout+stderr into the
spool file and into a bounded in-memory tail (`TAIL_BYTES`, 16 KiB), then
records the exit status on a `watch` channel. `bash` and `bash_wait` never
touch the child; they await the watch with a timeout and read the spool
from their last offset. This is what lets `wait_with_output` stop consuming
the child, which is why today's code cannot get a handle back after a
timeout.

The table is `Arc<Mutex<HashMap<JobId, Job>>>` owned by `BashTool` and
shared with the two new tools through `Arc` (they are constructed together
in `registry::build_tools`, gated by the same `bash` tool policy — a profile
that denies `bash` gets none of the three).

### 3.3 Bounds

| Bound | Default | Where | Why |
|---|---|---|---|
| concurrent jobs per agent | 8 | optional profile block `bash: { max_jobs: 8 }` (`tools:` is already the rule list, so the knob cannot live there) | a doom-looping model must not fork-bomb the host; the 9th `bash` gets `InvalidInput: 8 jobs running — bash_wait or bash_kill one first` |
| spool size | 64 MiB per job | const | log lands on disk; beyond this the spool truncates its head and says so |
| bytes per reply | 16 KiB tail | const | same figure `fleet_run` uses; `read_file` on the spool for more |
| result retention | 1 h after exit | const | abandoned handles are reaped; killed on runtime exit regardless |
| unattended deadline | inherited (`limits.deadline`) | already resolved by `bounds::resolve_bounds` | when the turn's deadline stops the loop, `graceful_exit` kills every job started by that task; an attended turn leaves them running (D3 of the parent spec: the human is the guard) |

No new global limit. `timeout_secs`'s cap is the only constant this spec
keeps, and it now bounds a wait, which is harmless.

### 3.4 Progress and stuck (D4)

`task_runner` feeds the stuck clock `(tool_name, fingerprint_args(input))`.
For a result whose status is `Running { bytes_seen, .. }` the runner mixes
`bytes_seen` into the fingerprint before `progress.observe`. Effect:

- `bash_wait` that returned 4 KiB of new test output → different
  fingerprint → progress.
- `bash_wait` three times on a job that printed nothing for 30 minutes →
  identical fingerprints → stuck clock runs → unattended run stops with
  `stuck: last calls bash_wait, bash_wait, bash_wait` and the jobs are
  killed by the deadline path. That is the correct verdict.

`Failed`/`Ok` results are unchanged.

### 3.5 What the model is told

The `bash` tool description becomes (verbatim intent, final wording in the
plan):

> Run a bash command. Waits up to `timeout_secs` (default 30, max 600) for
> it to finish. If it is still running after that, you get a `job_id` and
> the output so far — the command **keeps running**. Call `bash_wait` to
> wait longer, `bash_kill` to stop it. Pass `timeout_secs: 0` to start a
> command in the background immediately.

No skill or system-prompt change is required: an existing prompt that says
"use `timeout_secs: 600` for builds" still works and is now safe.

## 4. Error handling

| Situation | Result |
|---|---|
| spawn fails | `ToolError::Execution` as today (removable-volume EPERM hint kept) |
| child exits non-zero | `Failed { exit_code }`, text carries output + `[exit code: N]` as today; sandbox/write-denial attribution unchanged |
| unknown `job_id` | `ToolError::InvalidInput("no such job …; jobs running: j-…, j-…")` |
| `bash_wait` on a finished job | returns the retained result once, then the job is gone; a second wait is `InvalidInput` |
| job cap reached | `ToolError::InvalidInput` naming the running job ids |
| spool write fails (disk full, agent home not writable) | job continues with tail-only output; the reply says `[spool unavailable: <io error>]` — never kills the child for a logging failure |
| runtime shutdown | `kill_on_drop` on every child; no orphan (dogfood issue 11 preserved) |
| `bash_kill` on an already-exited job | `Ok`, text says it had already exited with code N |

Secrets: the spool file contains raw child output. It is written **through
the same masking chokepoint** the tool result uses (`SecretVault` mask on
every chunk before it reaches disk or the tail), because the model can
`read_file` the spool. This is the one place the design touches the secret
handoff spec (2026-09-07) and it must not regress it.

## 5. Testing

Unit, in `bash_jobs.rs` and `bash.rs`, real subprocesses (the existing
`run_capture_kills_a_wedged_process` test shows why: a threaded `Duration`
proves nothing):

1. `sleep 3` with `timeout_secs: 1` returns `Running` in ≈1 s **and the pid
   is still alive** afterwards — the assertion that distinguishes yield from
   kill.
2. `bash_wait` on that job with `wait_secs: 5` returns `Ok` with exit 0 and
   the job is gone from the table.
3. `bash_kill` → pid dead (`kill(pid, 0)` fails), status `Ok`, text names
   the elapsed time.
4. `timeout_secs: 0` returns `Running` with empty output before the command
   has printed anything.
5. Output that arrives after the yield is delivered by the next
   `bash_wait`, exactly once (offset bookkeeping).
6. Spool truncation at the cap says so in the text.
7. Job cap: the (N+1)th spawn is `InvalidInput` and names the others.
8. A secret exported to the child is masked in the spool file, not only in
   the reply.
9. Stuck fingerprint: two `Running` results with different `bytes_seen`
   produce different fingerprints; equal `bytes_seen` produce equal ones.
10. Registry: denying `bash` registers none of the three tools; allowing it
    registers all three.

Live verification (not CI): from murmur, ask an agent to run
`cargo build --release` in a repo it can reach; watch the settlement card
show `bash · still running` rather than `✘ timed out`, then `bash_wait`
land the exit code. The previous failure is the exact reproduction.

## 6. Rollout

One PR, one plan. Behaviour change worth a release-note line:
"`bash` no longer kills a command at `timeout_secs`; it returns a job
handle and the command keeps running. New tools `bash_wait`, `bash_kill`."
Agents pick it up on restart (`mur update --restart-agents`).

Follow-ups filed separately, not in this PR:

- murmur `!cmd` 30 s kill (`cli/stream.rs`) — stream the output and let
  Ctrl-C be the bound.
- LLM client request timeouts (`ollama.rs`, `openai.rs` 60 s) — make them
  idle timeouts between streamed chunks.

## 7. Out of scope

- Interactive stdin to a running job (Codex's `write_stdin`). Nothing in
  the failure needed it; add when a real command does.
- Surfacing jobs in `mur job status` / the Hub. A bash job is an in-process
  object; the run-status store lives in `mur-core`, which the runtime
  cannot depend on. If jobs need to be visible outside the agent, that is a
  new `RunKind` written by the runtime through `mur-open-items`-style
  shared crate — a separate design.
- Changing the MCP per-call timeout or `a2a_dial`; both already conform.
