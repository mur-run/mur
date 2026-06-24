# Dynamic Parallel Jobs — ephemeral fan-out without per-job workflows

- **Date:** 2026-06-24
- **Status:** Design (approved for plan)
- **Topic:** A `parallel_jobs` MCP action tool that fans N distinct ad-hoc jobs out to running agents over an ephemeral, in-memory DAG — no YAML/skill authored per job.

## Problem

Running N parallel implementation jobs today means authoring a `category:Workflow` skill (or workflow YAML) whose `content.procedure.steps` are rank-0 `delegate_to` steps, then `mur workflow run <skill>`. Authoring a static definition per fan-out is heavy and is exactly the per-job-static-template anti-pattern the 2026 literature says to leave behind.

The machinery to avoid it already exists: `mur-core/src/cmd/fleet/run.rs::build_fleet_procedure()` (lines 14–28) builds a `Procedure` **in memory** (one rank-0 `delegate_to` step per target) and runs it through `mur-core/src/executor/dag.rs::execute_dag()` — zero files. It only hardcodes *one shared goal broadcast to fleet members*. Generalizing it to **per-step descriptions with a free assignee** is the whole feature.

## Goals

- Fan **N distinct jobs** (each its own prompt) to **one running agent** as concurrent `channel/delegate` turns — the demo path, generalized, with no authored file.
- Triggerable by the **concierge mid-conversation** via an MCP tool (the concierge cannot shell `mur`).
- Bounded concurrency, fail-closed risk gating.
- The same primitive can also target **distinct named agents** per job (so "jobs across a fleet's members" works in v1 by naming the members) — see the fleet deep-think below.

## Non-goals (deferred)

- **Auto-distribute across a fleet** (`fleet` param + round-robin/router assignment). v1 targets agents by explicit name; auto-spreading N jobs over a fleet's member list is v2 sugar on top of the same primitive.
- **Typed per-job result envelope.** `execute_dag` returns one `PipelineOutput` whose `tokens_used` is the **summed** total and whose `output_text` is all step outputs concatenated — per-step results are **not** surfaced today (`StepResult` is private, `dag.rs:240`). v1 returns `{channel_id, output_text}` and lets the concierge read per-job replies off the channel. A structured `Vec<StepResult>` on `PipelineOutput` is a separate, deliberate change if ever wanted.
- **Router-assigned** jobs (LLM picks the best member per job). The planner exists (`cmd/fleet/plan.rs`); expose later as `assign:"router"`.
- **Fan-in verify step** (auto build/test gating merge). v2 — the concierge can issue a follow-up verify job today.
- **Budget ceiling.** A one-shot, concierge-triggered fan-out is attended (human in the loop), unlike the unattended `mur fleet run --loop`. The existing HITL gate is the brake in v1.
- **Parallel-edit merge reconciliation.** The 2026 survey confirms no framework has one; out of scope. Disjoint ownership is the discipline instead.

## Best-practice grounding (2026)

Filtered to what shapes this design:

- **Pre-execution generation, lowest plasticity that works.** "More flexibility is not intrinsically better" — for "N parallel coders" the structure is knowable, so generate one flat rank-0 DAG and fan out; do **not** do in-run spawn-as-you-go graph editing (RPI/IBM survey, arXiv 2603.22386).
- **Bounded concurrency is a hard precondition** — cap concurrency, not just cost: "15 concurrent agents consuming 150 req/s against a 100 req/s limit causes cascading failures" (Zylos, 2026-04-26). → `max_concurrency` below.
- **Disjoint ownership** — one file = one writer; "state corruption scales quadratically." MUR's `parallel-code` skill already encodes the gate (disjoint files, contracts frozen first).
- **Execution-grounded verify at fan-in** — run the build/tests, don't trust an LLM self-score (deferred to v2 here).

## Design

### 1. Core primitive + entry point (`mur-core`)

Lives in a small new `mur-core/src/executor/jobs.rs` (next to `execute_dag`, **not** under `cmd/fleet/` — the MCP tool is a separate crate and needs a `pub` path, and `dag.rs` is already >800 lines so it can't absorb more):

```rust
pub struct Job {
    pub description: String,   // the per-job prompt / task
    pub assignee: String,      // canonicalized agent name (delegate target)
}

/// One rank-0 ProcedureStep per job. Sets BOTH:
///   intent      = Some(description)  -> the delegate prompt   (dag.rs:474 = intent.unwrap_or(description))
///   description = description        -> channel/ledger/failure labels (dag.rs:573-586,855)
///   id          = "job-{i}"          -> stable+unique for idem-key / ToolResult crash-resume
///   depends_on  = []                 -> all rank-0, all parallel
pub fn build_jobs_procedure(jobs: &[Job]) -> Procedure

/// Public entry the MCP tool calls. Mints a throwaway channel, builds the
/// procedure, constructs DagExecOptions (owned backing strings — DagExecOptions<'a>
/// borrows trigger/env_class_override), runs execute_dag, returns (channel_id, output).
pub async fn run_parallel_jobs(
    mur_home: &Path,
    jobs: &[Job],
    max_concurrency: Option<usize>,
    yes: bool,
) -> Result<(String, PipelineOutput)>
```

Channel minted via the existing `ChannelService::create_for_workflow` path (used by `--channel-new`, `cmd/workflow.rs:196-197`). `run_parallel_jobs` passes `yes` straight to `DagExecOptions` and `run_id = format!("run-{}", Uuid::now_v7())`.

**`build_fleet_procedure` is left as-is** — its mapping differs (`intent = goal`, `description = "{member}: {goal}"`), so routing it through `build_jobs_procedure` would change the fleet delegate *prompt* from `goal` to `"{member}: {goal}"`. Behaviour-preserving > DRY; the ~12 shared lines aren't worth a silent prompt change.

### 2. Concurrency cap (the one executor change — 3 sites)

`execute_dag` spawns **all** same-rank steps via `tokio::task::spawn` then awaits them in a manual `for h in handles { h.await }` loop (`dag.rs:784` spawn, `:800-815` await — no `join_all`). Add an opt-in bound:

```rust
// DagExecOptions  (touch 3 sites or it won't compile)
//   (a) struct field:        pub max_concurrency: Option<usize>,
//   (b) Default impl:        max_concurrency: None,
//   (c) field-by-field reconstruction inside the spawn closure (dag.rs:785-794)
pub max_concurrency: Option<usize>,   // None = today's unbounded behaviour (back-compat)
```

When `Some(n)`, gate each spawned task on an `Arc<tokio::sync::Semaphore>` of `n` permits (`let _permit = sem.acquire().await;` inside each task, before the delegate dial). `None` preserves current behaviour exactly, so the fleet path and every existing caller are untouched. This is the research's hard precondition and it retro-fits fleet for free.

### 3. MCP tool `parallel_jobs` (`mur-mcp-server`)

The home flagged in prior project memory for `code_fanout`. `mur-mcp-server` already depends on `mur-core` (`Cargo.toml:14`), so reaching `run_parallel_jobs` is **not** a new dependency. It *is* the first **mutating** tool there (the current 9 are read-only) — see Integration risk #1.

```jsonc
// input
{
  "jobs": [ { "description": "string", "agent": "string?" } ],  // 1 ≤ len ≤ MAX_JOBS
  "agent": "string?",            // default assignee when a job omits `agent`
  "max_concurrency": 8,          // default; -> DagExecOptions.max_concurrency
  "yes": false                   // fail-closed; never blanket-approves risk-tiered steps
}
// output
{ "channel_id": "string", "output": "string" }   // concatenated step outputs; per-job replies live on the channel
```

Flow: validate input → resolve assignees (§4) → `run_parallel_jobs(home, &jobs, max_concurrency, yes)` → return `{channel_id, output}`. `MAX_JOBS` is a named `const` in the tool handler (an input guardrail, not a behaviour-shaping value — no config wiring).

### 4. Assignee resolution

A pure, unit-testable helper in `mur-core` (`resolve_jobs(raw, default_agent) -> Result<Vec<Job>>`), per job:

1. `job.agent` set → use it.
2. else top-level `agent` set → use it.
3. else → error (`"job N has no assignee: pass per-job `agent` or a top-level default `agent`"`).

All names go through `a2a_dial::canonicalize_agent_name` (`a2a_dial.rs:45`; case-insensitive, downstream uses the canonical name so the runtime spoof check passes).

### 5. Safety

- **Bounded concurrency** — §2 cap, default 8 from the tool.
- **Fail-closed** — `yes:false` always; risk-tiered (`risk: write`+) steps pause at the existing SHA-256-pinned HITL gate (`mur-core/src/hitl/`). The concierge cannot auto-approve destructive work.
- **Disjoint ownership** — the tool description references the `parallel-code` skill's gate (disjoint files, no shared registry/lockfile, contracts frozen read-only before fan-out). No merge-reconciler is built.
- **Input validation at the trust boundary** — MCP input is untrusted: enforce `1 ≤ jobs.len() ≤ MAX_JOBS`, non-empty descriptions, and a resolvable assignee for every job before any dial (fail-closed on any miss).

### 6. Result handling

v1 returns `{channel_id, output}` where `output` is the executor's concatenated `output_text` (`dag.rs:845-850`). The concierge reads **per-job** replies off `channel_id` (every delegate reply is a per-actor channel event with attribution). One failed job becomes an error entry in `output` and is recorded on the channel; it does **not** abort the batch — the executor isolates per-step failures (`on_failure`, per-task results in the await loop). This is the LangGraph "per-branch error marker, don't raise" rule.

## The "N distinct jobs → one fleet" deep-think (requested)

A fleet today = a named squad over one shared channel, run via `build_fleet_procedure(goal, members)` = **one goal broadcast to all members**. The user's "N distinct jobs → one fleet" is a different shape: **distinct work per member**.

Conclusion: this is **not a second mechanism** — it is `build_jobs_procedure` with assignees set to the fleet's member names. In **v1** you express it directly by passing per-job `agent` values that name those members. What's deferred to **v2** is only the *convenience*: a `fleet` param that reads `~/.mur/fleets/<name>/fleet.yaml`, validates membership, auto-spreads jobs across members (round-robin or router), and runs on the fleet's own channel (`fleet-<name>`) so the work lands in the fleet's audit trail and `active_fleet` scope-injection applies. No new types or executor path — just member-list resolution + the fleet channel. Cutting it from v1 removes a real trust-boundary surface (fleet-name validation, membership checks) for no loss of capability.

## Integration risks & gates (must clear in the plan)

1. **First mutating tool in `mur-mcp-server`.** The dependency on `mur-core` already exists (`Cargo.toml:14`); the open question is policy, not wiring: is it acceptable for the MCP process to execute delegations directly, or should it route through the daemon/action-pipeline? Decide explicitly in the plan.
2. **Target runtimes must be running and fresh.** `channel/delegate` needs each assignee's runtime alive and on a binary that registers `channel/delegate` (the demo's stale-Jun-1-binary `-32601`). Surface a clear, per-assignee error when a target isn't dialable.
3. **Coder fs-write entitlement must include `~/.mur/channels`** or the v3d-2 peer-self-reply silently drops from the audit trail (known bug, fixed for some agents). Note in the tool's guidance.

## Testing

- **Pure unit (`mur-core`):**
  - `build_jobs_procedure` — N jobs → N rank-0 steps; each step has `intent == Some(description)` **and** `description == job text` (regression guard for correction #4), unique `id`, empty `depends_on`, correct `delegate_to`.
  - `resolve_jobs` — per-job agent wins; falls back to default; errors when neither is set; canonicalization applied.
  - Input-validation rejections: 0 jobs, > MAX_JOBS, empty description.
- **Concurrency cap (`dag.rs`):** with `max_concurrency = Some(2)` and a counting stub delegate, peak in-flight ≤ 2; `None` preserves unbounded behaviour (regression guard for existing callers).
- **Live (operator):** real fan-out of distinct jobs to one running agent, and to two explicitly-named agents; needs runtimes + cc-proxy.

## File touch list (v1)

- `mur-core/src/executor/jobs.rs` (new, ~small) — `Job`, `build_jobs_procedure`, `resolve_jobs`, `run_parallel_jobs`, unit tests. Add `pub mod jobs;` to the executor module.
- `mur-core/src/executor/dag.rs` — `max_concurrency` on `DagExecOptions` (struct + `Default` + spawn-closure reconstruction) + semaphore in the rank loop.
- `mur-mcp-server/…` — register `parallel_jobs` tool: schema, `resolve_jobs`, `run_parallel_jobs` call, `{channel_id, output}` result, `const MAX_JOBS`. (Exact module mapped in the plan.)
- Tool description references the `parallel-code` skill gate.
