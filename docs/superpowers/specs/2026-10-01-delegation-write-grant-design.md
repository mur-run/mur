# Delegation write-grant check — design

**Status:** approved design, not yet implemented
**Fixes:** mur-run/mur#1607 (delegated agents leak other projects' build output into the `mur` repo root)
**Related:** #1602 (process cwd mistaken for session cwd), #1599

## 1. Problem

A delegating agent (fleet member, `parallel_jobs` assignee, workflow `delegate_to`
target) is handed a task about project *P*, but its sandbox write list
(`profile.yaml` → `entitlements.filesystem.write`) does not contain *P*. The
sandbox is compiled **once at agent start** —
`mur-agent-runtime/src/supervisor_runner/provider.rs:95`
`SandboxPolicy::from_entitlements(&profile.inner.entitlements, …)` — so every
write into *P* fails with `Operation not permitted`, and the model "recovers"
by writing build output and deliverables into the nearest directory it *can*
write: the `mur` repo root, which is on the list because that is where the
delegator lives.

No delegation path checks this today:

| Path | Entry | How the target dir reaches the member | Write check |
|---|---|---|---|
| `mur fleet run` | `mur-core/src/cmd/fleet/run.rs:385` | `discover_repo_root()` result pasted into the goal as prose (`run.rs:388` "IMPORTANT: the repository you are working on is at …") | none |
| `parallel_jobs` (MCP tool) | `mur-mcp-server/src/tools.rs:751` → `mur-core/src/executor/jobs.rs:28 build_jobs_procedure` | **not at all** — the member gets only `description` | none |
| workflow `delegate_to` | `mur-core/src/executor/dag.rs:855` | not at all | none |

`delegate_to` only dials a member on a **channel run**: the delegation branch at
`dag.rs:855` needs both a target and a channel id. A run without `--channel` /
`--channel-new` falls through to intent mode (`dag.rs:743`), prints the step
description, and reports `exit_code: 0` — no member is called, so it carries no
write risk. Only channel runs need the gate.

Two compounding bugs in how the target directory is *discovered*:

- `fleet_run.rs:243` spawns `mur fleet run` with no `.current_dir(…)`, so
  `discover_repo_root()` (`run.rs:199`, `git rev-parse --show-toplevel`) runs in
  the **runtime process cwd**, not the user's session cwd. Same shape as #1602.
- The MCP server is a separate process with no notion of session cwd
  (`grep cwd mur-mcp-server/src` is empty), and the runtime forwards tool input
  verbatim (`mur-agent-runtime/src/tools/mcp.rs:115 call_tool(&self.tool, input)`).
  `parallel_jobs` therefore cannot even guess.

The author already left the hook: `run.rs:623-635` "Tier 1 routing is
best-effort; strays justify Tier 2 enforced cwd." This spec is Tier 2.

## 2. Decisions (recorded from the brainstorm)

| # | Question | Decision |
|---|---|---|
| D1 | What happens when the member cannot write the target? | **Block, then grant via HITL.** Refuse to dispatch; raise one HITL gate asking to add `<dir>` to `<member>`'s write list. Approve → grant → dispatch. Deny / unattended → block. Never auto-grant (would hollow out the entitlements pin). |
| D2 | Where does the target dir for `parallel_jobs` / `delegate_to` come from? | **Explicit `cwd` first, session cwd as a *degraded* fallback.** Explicit `cwd` → check runs silently, fast path. No `cwd` → the runtime injects the session cwd, flags it `cwd_inferred: true`, and the check **always** goes through HITL ("no target directory was given; I inferred `<session_cwd>` — proceed?"), even when the member already has write access. Deny → block, tell the caller to pass `cwd`. |
| D3 | Who performs profile edit → reseal → restart after approval? | **MUR does all three, in-process, in that order** — *if* the member runs as a managed service (launchd `KeepAlive`, `service.rs:probe_service`). `restart_quiet` (`mur-core/src/cmd/agent/restart.rs:334`) is already graceful: SIGTERM, drain the in-flight turn, wait for a fresh `running.lock` with a different pid. Only after the new pid is up does fan-out proceed. |
| D3b | Member is **not** a service? | **Degrade honestly:** edit + reseal, then block this dispatch and print `mur agent restart <member>`. The HITL copy changes to say so (§4.3). Never spawn the member ourselves — we do not know how the user brought it up (tmux, nohup, foreground, another shell); guessing risks a double start or an orphan. |
| D4 | Where does the check live? | **One shared pre-dispatch gate, called before fan-out,** so fleet / parallel_jobs / delegate_to share it and N jobs to the same member trigger one grant, not N racing ones. |

Why D2's fallback must be HITL and not just a log line: the "assumed from
session cwd" note is read by an agent, not a person, and an agent does not stop
on a note. The only reader we trust to catch a wrong guess is the human. This
also keeps the fleet safety triad: unattended HITL **defers**, never times out,
so a guessed directory is never silently written to.

## 3. Architecture

```
caller (model) ──► tool call ──► runtime (owns SessionCwd)
                                  │  inject cwd if absent, set cwd_inferred
                                  ▼
                       mur-mcp-server / mur fleet run   (mur-core)
                                  │
                                  ▼
             executor::delegation::grant::ensure_write_grants(plan, opts)
                                  │  1. dedupe (member, target)
                                  │  2. for each: check / HITL / grant / restart
                                  ▼
                   dispatch_parallel_jobs / fleet fan-out / delegate step
```

Everything below the runtime lives in `mur-core`; `restart_quiet`, reseal
(`cmd/agent/perm.rs:165`), and the profile writer are all already there, so no
new crate boundary is crossed. The *check* itself needs the sandbox's own path
expansion so it agrees with what the member will actually enforce —
`mur-agent-runtime::sandbox::policy::expand_entitlement_path` (`policy.rs:60`)
and `fs_policy::under_any_or_worktree`. `mur-core` already depends on
`mur-agent-runtime` (`mur-core/Cargo.toml:24`), so reuse them; do not
re-implement glob/`~` expansion.

## 4. Components

### 4.1 Runtime: target-dir injection (`mur-agent-runtime`)

- **`tools/mcp.rs` (`McpTool::execute`)** — for tools in a small allowlist
  (`parallel_jobs`, workflow-run tools that accept `delegate_to` steps): if
  `input` has no `cwd`, insert `cwd = SessionCwd::current()` and
  `cwd_inferred = true`. Never override an explicit `cwd`. Allowlist is a
  constant next to the tool names, not a scan of every MCP tool.
- **`mur-core/src/executor/delegation/cwd.rs`** (shared) — `RunCwd`,
  `discover_repo_root(from)`, `routing_note(dir, inferred)`. Both `mur fleet
  run` and `parallel_jobs` build the member prompt through it, so the two
  paths cannot drift. `RunCwd::from_tool_args` treats an absent `cwd` at a tool
  boundary as inferred (the serving process's cwd is a guess there).
- **`tools/fleet_run.rs`** — pass the session cwd explicitly: add
  `--cwd <path>` to the spawned `mur fleet run` (or `.current_dir(…)`; prefer
  the flag so it is visible in the job log) and `--cwd-inferred` when the
  model gave none. `discover_repo_root()` takes the path instead of relying on
  process cwd.
- The `fleet_run` and `parallel_jobs` tool schemas gain an optional `cwd`
  (string, absolute path) so a model *can* be explicit. Description text tells
  it to pass the target project, not where it happens to be sitting.

### 4.2 Core: the gate (`mur-core/src/executor/delegation/grant.rs`, new)

```rust
pub struct DelegationTarget { pub member: String, pub dir: PathBuf, pub inferred: bool }

pub enum GrantOutcome {
    AlreadyAllowed,                 // fast path, nothing written
    Granted { restarted: bool },    // profile edited + resealed (+ restarted)
    Blocked(BlockReason),           // dispatch must not proceed
}

pub struct GrantContext<'a> {
    pub mur_home: &'a Path,
    pub channel_id: &'a str,   // where approvals park and outcomes are recorded
    pub run_id: &'a str,
    pub policy: GatePolicy,    // yes / unanswered / auto_approve_tiers
    pub job_count: usize,      // quoted in the prompt
}

pub async fn ensure_write_grants(
    ctx: &GrantContext<'_>, targets: &[DelegationTarget],
) -> Result<Vec<(DelegationTarget, GrantOutcome)>>;
```

The target `dir` is always the **routing target** — the git root of the work
dir (`cwd::routing_target`), i.e. the directory the routing note sends the
member to. Checking the raw cwd instead would pass a member that may only
write a subdirectory while sending it to the root.

Per unique `(member, canonical dir)`:

1. **Canonicalize** the dir; non-existent → `Blocked(TargetMissing)`.
2. **Load** the member's `profile.yaml`, expand `filesystem.write` with
   `expand_entitlement_path`, test with `under_any_or_worktree`. Also test
   `filesystem.deny`; a denied target is `Blocked(Denied)` — we never offer to
   grant past a deny.
3. **Decide whether HITL is needed:** `!allowed || inferred`.
   `allowed && !inferred` → `AlreadyAllowed`.
4. **Raise one HITL gate** through the existing channel HITL
   (`mur_common::hitl`, via the caller's `GatePolicy` — see the site table below).
   Risk tier is `write`-equivalent, so `--yes` / `yes=true` /
   `auto_approve_tiers` containing `write` may satisfy it (capped by
   `tier_may_be_granted`, consistent with the rest of HITL). Unattended with no
   auto-approve → defer, return `Blocked(Deferred { hitl_id })`.
5. **On approval, if `!allowed`:** append the dir to `filesystem.write` via the
   atomic YAML writer (`store/yaml.rs`) → reseal (factor the body of
   `cmd_perm_reseal` into a callable that returns the diff it printed) →
   if `probe_service(member)` says installed, `restart_quiet(member)` and wait
   for the new pid → `Granted { restarted: true }`. Not a service →
   `Granted { restarted: false }`, which the caller treats as **block this
   dispatch** and prints `mur agent restart <member>`.
6. **On approval, if `allowed && inferred`:** nothing to write; return
   `AlreadyAllowed`.

The gate needs a channel (approvals park there; outcomes are recorded there
as `delegation.write_grant`), so each site **creates or resolves its channel
first, then gates, then dispatches**. A blocked run leaves a channel holding
only its `delegation.write_grant` records and the parked request — that is the
audit trail, not litter. One restart per member per run, never one per job.

| Site | Channel | Target | Policy | When blocked |
|---|---|---|---|---|
| `parallel_jobs` (`jobs.rs` `dispatch_parallel_jobs`, now `async`) | minted per call | caller `cwd` (inferred if the runtime injected it) | `yes` from the call; unanswered **Defer** (a tool call has no human on stdin) | tool error naming `mur channel approve <channel> <hitl>` |
| `mur fleet run` (one-shot, `run.rs`) | the fleet's channel | `--cwd` (`--cwd-inferred` when the tool supplied it) | `yes: false` always; fleet `hitl:` mode and `auto_approve_tiers` | bail before worktree creation; the claimed job is marked failed with the reason |
| `mur workflow run` (`mur-core/src/executor/delegation/workflow.rs` `prepare_procedure`) | `--channel` / `--channel-new` only | repo root of the invocation dir, **always inferred** (no `--cwd` flag) | same as the DAG's own gates: `--yes`, TTY-aware `default_unanswered` | bail before the first step; on pass, the routing note is appended to every delegate step's prompt |

A workflow run **without** a channel is not gated (nothing is dialled); if any
step has `delegate_to`, it warns that those members will not be called and
names `--channel-new` / `--channel <id>`. That a channel-less delegate step
reports success while doing nothing is a separate defect, tracked on its own.

**Not covered: `mur fleet run --loop`** (and its callers `fleet_tick` and
`deep-research`). `cmd_fleet_run_loop` takes no cwd at all
(`dispatch.rs:371`), so where its members work was never defined. Gating it
first needs a `cwd` parameter on the loop and its daemon / deep-research
callers — a routing change, done in a follow-up PR, then gated once before
the loop starts.

### 4.3 HITL copy (the user signs a *complete* action)

| Case | Prompt |
|---|---|
| not allowed, service | "Add `<dir>` to `<member>`'s filesystem write list and restart `<member>` (waits for its current turn to finish)? Required to dispatch `<n>` job(s) for this run." |
| not allowed, **not** a service | "Add `<dir>` to `<member>`'s filesystem write list. `<member>` is not running as a service, so MUR will not restart it: after resealing you must run `mur agent restart <member>` yourself, and **this dispatch will fail**. Continue?" |
| allowed, inferred | "No target directory was given for this delegation. Inferred `<dir>` from the session working directory — dispatch `<n>` job(s) against it?" |
| not allowed, inferred | Combine: the inferred-dir sentence first, then the grant sentence for the matching service case. One gate, not two. |

Prompts carry `member`, `dir`, `inferred`, `service` as structured fields on
the HITL event so the Hub / mobile can render them, and so `action_hash`
matching (deferred unattended gates) is stable across retries.

### 4.4 Observability

- Every gate decision emits a `tracing` event and a channel event
  `delegation.write_grant` with `{member, dir, inferred, outcome}`.
- `Blocked` reasons surface verbatim in the tool result / `mur fleet run`
  output, with the fix line (`pass cwd=…` or `mur agent restart …`).
- The job log for a fleet run records the `--cwd` it was given and whether it
  was inferred, so a stray build dir can be traced to its dispatch.

## 5. What this does not do

- **No hot-reload of the sandbox.** Restart stays the only way to apply
  entitlements; this spec does not add a SIGHUP path. Reason: the sandbox
  compiles once by design (`provider.rs:95`); reloading mid-turn would let a
  running turn gain permissions it was not started with.
- **No automatic grant without HITL**, under any flag except the existing
  write-tier auto-approve knobs, which are already user-configured and capped.
- **No spawning of non-service members** (D3b).
- **No change to the entitlements pin format.** Reseal is the existing
  mechanism; we call it, we do not bypass it.
- **No `read` grants.** Only `filesystem.write` is in scope; a member that
  cannot *read* the target fails loudly already and does not stray.

## 6. Testing

- Unit (`grant.rs`): allowed/explicit → no HITL; allowed/inferred → HITL;
  denied-list → `Blocked(Denied)` with no HITL; three jobs to one member →
  one gate, one reseal; service vs non-service → `restarted` flag and the two
  different prompts.
- Integration (`parallel_jobs` through a fake MCP client): omit `cwd` → the
  runtime injects `SessionCwd::current()` and `cwd_inferred: true`; explicit
  `cwd` is never overwritten.
- `fleet_run` tool: spawned command line contains `--cwd <session cwd>`; the
  runtime process cwd is irrelevant (set it to `/` in the test).
- Regression for #1607: member profile lacks `<tmp project>`; dispatch with
  explicit `cwd` and a denying HITL answer → nothing written anywhere, profile
  unchanged, exit status says blocked.
- Lint per CLAUDE.md: `cargo clippy --all --all-targets --no-deps --locked -- -D warnings`.

## 7. Rollout

1. `fleet_run` `--cwd` plumbing + `discover_repo_root(path)` — removes the
   process-cwd guess on its own, safe to ship first. **Done.**
2. Runtime injection for `parallel_jobs` + schema `cwd`; routing note
   appended to each job's prompt via the shared `delegation::cwd`. **Done.**
3. `grant.rs` gate wired into the three dispatch sites (`parallel_jobs`,
   one-shot `mur fleet run`, channel-mode `mur workflow run`), HITL copy,
   tests. **Done.**
3b. `mur fleet run --loop` (`fleet_tick`, `deep-research`): add a `cwd` to the
   loop and its callers, route members with it, gate once before the loop.
   Follow-up PR.
4. Docs: `README.md`, docs site, product page via the `update-docs` skill;
   `mur fleet run --help` and the `parallel_jobs` tool description mention
   `cwd`.
