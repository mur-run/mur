# bash: a timeout is a yield, not a kill

**Status:** Implemented in #1288. Live verification (spec §5) still pending — see that PR's description.
**Scope:** `mur-agent-runtime` (`tools/bash.rs`, new `tools/bash_jobs.rs`, `tools/registry.rs`, `secrets.rs` streaming masker, `task_runner.rs` policy alias + task-local owner + deadline/cancel cleanup + fingerprint, `turn_ledger.rs` running outcome) and `mur-core` (murmur `CallOutcome::Running` rendering only). No protocol change on the wire beyond one additive boolean on the existing `ToolResult` event. No new config keys.
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
| `mur-core/src/cmd/agent/cli/stream.rs:103` | murmur `!cmd` 30 s, kills | same disease, user-facing; follow-up #1286 |
| `mur-agent-runtime/src/llm/{ollama,openai}.rs` | 60 s whole-request timeout, applies to streamed bodies too | a slow local model producing a long answer is cut off; follow-up #1287 |
| `mur-common/src/agent.rs` `hitl.timeout_secs` 300 | approval wait | already defers instead of timing out unattended (2026-08-19 spec); untouched |

## 2. Decisions

| # | Decision | Rejected alternative |
|---|---|---|
| D1 | **A timeout is a yield.** When `timeout_secs` elapses and the child is still running, `bash` returns a handle and the child keeps running. Nothing is killed by the clock. | Kill at T and add `background: true` (Claude Code's model) — the model must guess in advance which commands are slow; a wrong guess still loses the work. That guess is exactly what failed here. |
| D2 | **No ceiling on work, a ceiling on waiting.** `timeout_secs` keeps its name and its 600 s cap, but the cap now bounds only how long one tool call holds the turn. A job may run for hours across many `bash_wait` calls. | Make `MAX_TIMEOUT_SECS` configurable — moves the number into YAML and keeps the kill semantics; the parent spec's "don't just raise it". |
| D3 | **Jobs outlive the turn.** A running job survives the end of the model's turn so the agent can say "build is running, I'll check" and answer a later message with `bash_wait`. Jobs die with the runtime process, with `tasks/cancel` of the task that started them, and with an unattended deadline/stuck stop of that task. | Kill all jobs at turn end — forces the model to babysit every long command inside one turn, which is the blocking model with extra steps. |
| D4 | **A wait that returned new output is progress.** The stuck clock fingerprints tool calls by arguments; `bash_wait {job_id}` repeated would look stuck. The runtime folds the byte offset the wait reached into that fingerprint, so a wait that delivered bytes differs from the previous one and a wait that delivered nothing repeats. | Ask the model to pass a `since` cursor — pushes bookkeeping onto the model and it can pass the same value forever. |
| D5 | **Running is a status, not an error.** `ToolStatus::Running { job_id, bytes_seen }` joins `Ok / Failed / Denied`. A yielded call is `is_error: false`. The text says *still running*, never *timed out*. The ledger and murmur render it as a fourth state (§3.7), not as success. | Return `Err(ToolError::Execution("timed out"))` with the job id in the message — the structural status is what the model and the settlement card read; text markers were already removed once (`ToolStatus` doc comment). |
| D6 | **Two new tools, no new arguments to learn.** `bash_wait { job_id, wait_secs }` and `bash_kill { job_id }`. `bash` gains nothing except the changed meaning of `timeout_secs`; `timeout_secs: 0` means "return the handle at once" (background). `wait_secs` is accepted as an alias for `timeout_secs` on both tools. | One `bash` tool with `mode: start|wait|kill` — three behaviours behind one schema is harder for the model to read than three names. |
| D7 | **Output spools to disk, the model sees the tail.** Combined stdout+stderr streams to `<agent_home>/jobs/<job_id>.log`; each reply carries the bytes since the last reply, capped, plus the file path so the model can `read_file` any range. | Keep everything in memory — a 200 MB build log inside the runtime process. |
| D8 | **Every job has an owner task, delivered by a tokio task-local.** Tools are built once per runtime (`supervisor_runner.rs:342`) and `ToolExecutor::execute` receives only JSON, so the runner scopes `CURRENT_TASK_ID` around both execute sites and `bash` reads it at spawn. `JobTable::kill_owned_by(task_id)` is what deadline, stuck and cancel call. | Add a context parameter to `ToolExecutor::execute` — touches every built-in and the MCP wrapper for one consumer. Inject `_task_id` into the input JSON — pollutes the fingerprint and the model-visible schema. |
| D9 | **A job is a process group.** The child is spawned with `process_group(0)`; `bash_kill`, deadline cleanup and runtime shutdown signal the **group** (`SIGTERM`, then `SIGKILL` after a 2 s grace), so `cargo`, test binaries and pipeline stages die with their shell. `kill_on_drop` stays as the belt. | Kill only the direct `bash` — its grandchildren keep running and keep the CPU; the failure this spec is for would come back as an orphaned `cargo test`. |
| D10 | **Masking is streaming and boundary-safe.** A `StreamMasker` built from the vault holds back `longest_secret_bytes - 1` bytes (plus any incomplete UTF-8 tail) between reads and flushes at EOF, so a secret split across two pipe reads is still caught before it reaches the spool or the tail. Both sinks consume only masked bytes. | Mask each chunk independently — a secret split `abc|def` reaches disk in the clear, and the model can `read_file` the spool. |
| D11 | **`bash_wait` and `bash_kill` inherit `bash`'s tool rule.** `effective_tool_policy` resolves them by their own exact name first; with no explicit rule they resolve as `bash`. An `always allow bash` written by murmur/Hub therefore covers all three. | Register them under the `bash` gate only — registration happens once, but the gate at execution resolves the called name, so `bash: allow` would leave the two new tools at `Ask` every call. |
| D12 | **Concurrency cap is a runtime constant, not a profile key.** `MAX_JOBS = 8` per agent. | A `bash: { max_jobs }` profile block — a `mur-common` schema change, fixture and Hub surface for a knob nobody has asked to turn. Promote it when someone does. |

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
// → Ok { text: "killed j-01J… after 9m12s (SIGTERM, then SIGKILL); last output:\n…" }
```

Defaults: `bash.timeout_secs` 30 (unchanged), `bash_wait.wait_secs` 60,
both clamped to `[0, 600]`. The 600 s cap is the **wait** cap; the tool
description says so in one sentence and never uses the word "killed" for
the clock.

`timeout_secs: 0` on `bash` spawns and returns `Running` immediately —
the background case, with no separate flag.

### 3.2 Job lifecycle

```
bash(cmd) ──spawn(process_group)──▶ Job { id, owner_task_id, pgid, child, spool, tail,
                │                         started_at, exit: watch<Option<ExitStatus>> }
                ├─ finished within timeout ──▶ Ok/Failed, job removed from table
                │
                └─ still running ──▶ Running{job_id}; job stays in table
                                          │
                     bash_wait ◀──────────┤  (any later turn, any task)
                     bash_kill ◀──────────┤
                     kill_owned_by(task) ◀┤  (deadline / stuck / tasks/cancel)
                                          │
                     finished ──▶ result held in table until one bash_wait
                                  collects it, then removed (or reaped after
                                  JOB_RESULT_TTL, default 1 h, so an
                                  abandoned handle does not leak)
```

One tokio task per job owns the child: it reads stdout+stderr, pushes every
chunk through the `StreamMasker` (D10), and writes the masked bytes to the
spool file and to a bounded in-memory tail (`TAIL_BYTES`, 16 KiB), then
records the exit status on a `watch` channel. `bash` and `bash_wait` never
touch the child; they await the watch with a timeout and read the spool
from their last offset. This is what lets `wait_with_output` stop consuming
the child, which is why today's code cannot get a handle back after a
timeout.

The table is `Arc<JobTable>` (a `Mutex<HashMap<JobId, Job>>` plus the kill
helpers) owned by `BashTool` and shared with the two new tools and with
`TaskRunner` (`with_bash_jobs(table)`). The three tools are constructed
together in `registry::build_tools`; a profile that denies `bash` gets none
of them.

### 3.3 Owner task (D8)

```rust
tokio::task_local! { pub static CURRENT_TASK_ID: String; }
// task_runner, one helper used by BOTH execute sites (the Allow path and
// the post-approval path — the pair that has bitten twice):
CURRENT_TASK_ID.scope(task_id.clone(), tool.execute(input)).await
```

`bash` reads `CURRENT_TASK_ID.try_with(..)` at spawn; `None` (tests,
embedded use) records `owner: None`, which only the runtime shutdown path
kills. `bash_wait`/`bash_kill` do **not** check ownership — a later turn of
the same agent is a different task id and must be able to collect the
result (D3). Ownership exists for cleanup, not for access control; every
job belongs to one agent process anyway.

Cleanup calls, all `JobTable::kill_owned_by(&task_id)`:

- the loop stops with `LoopStop::Deadline` or `LoopStop::Stuck` (unattended
  only — attended never stops on those);
- `tasks/cancel` for that task;
- runtime shutdown: `JobTable::kill_all()` from the supervisor's stop path,
  before `kill_on_drop` would.

### 3.4 Process group (D9)

Unix: `Command::process_group(0)` (std `CommandExt`, available through
tokio's `Command`), `pgid == child pid`. Kill sequence:
`killpg(pgid, SIGTERM)` → wait up to `KILL_GRACE` (2 s) on the exit watch →
`killpg(pgid, SIGKILL)`. The reader task reaps the direct child; group
members are reparented to init and reaped there.

Windows: `taskkill /F /T /PID <pid>` — a tree kill, so grandchildren die
there too (`ponytail:` note in code names Job Objects as the upgrade if
an orphan is ever reported).

The seatbelt sandbox on macOS is not expected to deny `setpgid`; the live
verification (§5) confirms it inside a real sealed agent, because a comment
saying so is not evidence.

### 3.5 Streaming masker (D10)

`SecretVault::masker() -> StreamMasker`. `push(&[u8]) -> Vec<u8>` returns
the bytes that are safe to emit; `finish() -> Vec<u8>` flushes the carry.
Carry = the last `max(longest secret value in bytes) - 1` bytes of the
concatenated input, extended backwards to a UTF-8 boundary. Everything
before the carry is masked with the existing `mask` and emitted. Secrets can
be added while a job runs (`secret/set`), so the carry length is re-read
from the vault on every `push`. No secrets → passthrough with zero copies.

The existing whole-result `masked()` chokepoint in `task_runner` stays; the
tail and spool are already masked, so it is a no-op there and still guards
every other tool.

### 3.6 Bounds

| Bound | Default | Where | Why |
|---|---|---|---|
| concurrent jobs per agent | 8 | `MAX_JOBS` const (D12) | a doom-looping model must not fork-bomb the host; the 9th `bash` gets `InvalidInput: 8 jobs running — bash_wait or bash_kill one first` |
| spool size | 64 MiB per job | const | log lands on disk; past the cap the spool stops growing, the tail stays live, and the reply says so |
| bytes per reply | 16 KiB tail | const | same figure `fleet_run` uses; `read_file` on the spool for more |
| result retention | 1 h after exit | const | abandoned handles are reaped; killed on runtime exit regardless |
| kill grace | 2 s | const | SIGTERM → SIGKILL on the group |
| unattended deadline / stuck | inherited (`limits.*`) | already resolved by `bounds::resolve_bounds` | stop → `kill_owned_by(task)`; an attended turn leaves jobs running (parent D3: the human is the guard) |

No new global limit. `timeout_secs`'s cap is the only constant this spec
keeps, and it now bounds a wait, which is harmless.

### 3.7 Progress, stuck, and what the user sees

**Stuck clock (D4).** `task_runner` feeds `(tool_name, fingerprint_args(input))`.
For a result whose status is `Running { bytes_seen, .. }` the runner mixes
`bytes_seen` into the fingerprint before `progress.observe`. A `bash_wait`
that returned 4 KiB of test output is progress; three waits on a job that
printed nothing for 30 minutes are identical, the unattended run stops with
`stuck: last calls bash_wait, bash_wait, bash_wait`, and `kill_owned_by`
ends the job. That is the correct verdict.

**Settlement ledger.** `turn_ledger::Outcome` gains `Running(String)`
(detail: the job id — elapsed lives in the tool's own reply text, not
duplicated onto the ledger `Action`). `classify` maps
`ToolStatus::Running` to it before the `is_error` check. The card
renders it as its own line —
`⏳ bash · still running (j-01J…) — bash_wait to continue` — and the
summary counts running separately from ✔ and ✘.

**murmur.** The `ToolResult` event already carries `ok` and `denied`; it
gains `running: bool` (additive, old readers ignore it). `CallOutcome`
gains `Running`, mapped to a `StepState::Yielded` (named apart from the
existing `Running` spinner state — that one means "the runtime has not
answered yet") that renders as ⏳ and is neither `Done` nor `Error`. A step
that later completes via `bash_wait` is a different call and a different
card; the running card stays as the record of the yield.

### 3.8 Policy (D11)

```rust
fn policy_name(tool: &str) -> &str {
    match tool { "bash_wait" | "bash_kill" => "bash", other => other }
}
// effective_tool_policy: explicit rule for the called name wins;
// otherwise resolve policy_name(tool); otherwise today's defaults.
```

The HITL risk tier on the rule (`ToolRule.risk`) is inherited the same way.
A user who wants `bash_kill` gated separately writes an exact rule for it.

### 3.9 What the model is told

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
| `process_group(0)` refused (unexpected seatbelt rule) | spawn fails with the error naming `setpgid`; never silently falls back to an ungrouped child, because then `bash_kill` would lie |
| child exits non-zero | `Failed { exit_code }`, text carries output + `[exit code: N]` as today; sandbox/write-denial attribution unchanged |
| unknown `job_id` | `ToolError::InvalidInput("no such job …; jobs running: j-…, j-…")` |
| `bash_wait` on a finished job | returns the retained result once, then the job is gone; a second wait is `InvalidInput` |
| job cap reached | `ToolError::InvalidInput` naming the running job ids |
| spool write fails (disk full, agent home not writable) | job continues with tail-only output; the reply says `[spool unavailable: <io error>]` — never kills the child for a logging failure |
| runtime shutdown | `kill_all()` signals every group, then `kill_on_drop`; no orphan (dogfood issue 11 preserved, now including grandchildren) |
| `bash_kill` on an already-exited job | `Ok`, text says it had already exited with code N |
| SIGTERM ignored | SIGKILL to the group after `KILL_GRACE`; the text says which signal ended it |

## 5. Testing

Unit, in `bash_jobs.rs`, `bash.rs`, `secrets.rs`, `task_runner.rs`,
`turn_ledger.rs`, real subprocesses where a process is the subject (the
existing `run_capture_kills_a_wedged_process` test shows why: a threaded
`Duration` proves nothing):

1. `sleep 3` with `timeout_secs: 1` returns `Running` in ≈1 s **and the pid
   is still alive** afterwards — the assertion that distinguishes yield from
   kill.
2. `bash_wait` on that job with `wait_secs: 5` returns `Ok` with exit 0 and
   the job is gone from the table.
3. `bash_kill` → pid dead (`kill(pid, 0)` fails), status `Ok`, text names
   the elapsed time and the signal.
4. **Grandchild:** `bash -c 'sleep 60 & echo $!; wait'` — capture the
   grandchild pid from the output, `bash_kill`, assert the grandchild is
   dead within `KILL_GRACE + 1 s`. The regression D9 exists for.
5. `timeout_secs: 0` returns `Running` with empty output before the command
   has printed anything.
6. Output that arrives after the yield is delivered by the next
   `bash_wait`, exactly once (offset bookkeeping).
7. Spool truncation at the cap says so in the text.
8. Job cap: the (N+1)th spawn is `InvalidInput` and names the others.
9. **Masker, every split point:** for a vault with secret `S` and input
   `"pre S post"`, for every `k in 0..=len`, feed `[..k]` then `[k..]` then
   `finish()`; assert `S` never appears in the concatenated output and
   `[SECRET:NAME]` appears exactly once. Repeat with a multi-byte character
   adjacent to the split. Repeat with two secrets where one is a prefix of
   the other.
10. Masker end-to-end: a secret exported to the child and `echo`ed by it is
    masked in the spool **file**, not only in the reply, with the pipe read
    size forced to 1 byte.
11. **Ownership:** two concurrent `CURRENT_TASK_ID` scopes each spawn a
    job; `kill_owned_by(A)` kills A's and leaves B's alive; `kill_owned_by`
    of an unknown task kills nothing. A job spawned outside any scope is
    killed only by `kill_all`.
12. Deadline path: an unattended loop that stops on `Deadline` leaves no
    job of that task alive; an attended loop that ends normally leaves the
    job running.
13. `tasks/cancel` kills that task's jobs.
14. Stuck fingerprint: two `Running` results with different `bytes_seen`
    produce different fingerprints; equal `bytes_seen` produce equal ones.
15. Policy: `bash: allow` → `bash_wait`/`bash_kill` allow; `bash: ask` →
    ask; `bash: allow` + `bash_kill: deny` → deny for kill only; no rule →
    today's default.
16. Registry: denying `bash` registers none of the three tools; allowing it
    registers all three.
17. Ledger: `classify` on `ToolStatus::Running` → `Outcome::Running`, and
    the rendered card contains `still running` and not `✘`.
18. murmur: a `ToolResult` event with `running: true` renders a
    `StepState::Running` card; one without the field renders as before.

Live verification (not CI): from murmur, ask a sealed agent to run
`cargo build --release` in a repo it can reach; watch the card show
`bash · still running` rather than `✘ timed out`, then `bash_wait` land the
exit code; `bash_kill` a `cargo build` and confirm with `pgrep` that no
`rustc` survives. The previous failure is the exact reproduction; the
`pgrep` is the D9 and seatbelt check.

## 6. Rollout

One PR, one plan. Behaviour change worth a release-note line:
"`bash` no longer kills a command at `timeout_secs`; it returns a job
handle and the command keeps running. New tools `bash_wait`, `bash_kill`."
Agents pick it up on restart (`mur update --restart-agents`).

Follow-ups, filed: #1286 (murmur `!cmd` 30 s kill), #1287 (LLM client
whole-request timeouts).

## 7. Out of scope

- Interactive stdin to a running job (Codex's `write_stdin`). Nothing in
  the failure needed it; add when a real command does.
- Surfacing jobs in `mur job status` / the Hub. A bash job is an in-process
  object; the run-status store lives in `mur-core`, which the runtime
  cannot depend on. If jobs need to be visible outside the agent, that is a
  new `RunKind` written by the runtime through a shared crate — a separate
  design.
- Windows process-tree termination (Job Objects) — D9 ceiling.
- A configurable job cap — D12.
- Changing the MCP per-call timeout or `a2a_dial`; both already conform.

## 8. Review log

2026-09-12, six findings, each checked against main before the design
changed:

| Finding | Verified at | Resolution |
|---|---|---|
| chunk-wise masking leaks a split secret | `task_runner.rs:668` masks whole results only | D10, §3.5, tests 9–10 |
| `bash: { max_jobs }` breaks the runtime-only scope | `AgentProfile` has no such field | D12 — constant |
| `bash: allow` leaves the new tools at Ask | `effective_tool_policy` resolves the called name | D11, §3.8, test 15 |
| deadline cleanup has no owner | tools built once in `supervisor_runner.rs:342`; `execute` takes JSON only; `graceful_exit` has no table | D8, §3.3, tests 11–13 |
| killing `bash` orphans `cargo` | no `process_group` on the child | D9, §3.4, test 4 |
| Running renders as success | `turn_ledger::classify` special-cases Denied only; murmur `CallOutcome` has three variants | §3.7, tests 17–18, scope widened to mur-core |
