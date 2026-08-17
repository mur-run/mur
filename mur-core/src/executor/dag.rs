//! Unified DAG executor for `category: Workflow` skills (workflow-engine v2 P3).
//!
//! Loads a `Procedure` (from a skill's `ProcedureStep` list), topo-sorts by
//! `depends_on`, groups steps by topological rank, and executes each rank
//! concurrently via `tokio::spawn`. Command-mode steps run via `sh -c`;
//! intent-mode steps print instructions and mark `skipped_intent` in the
//! ledger. Every step writes a run-ledger record via `record_run`.
//!
//! The existing `PipelineExecutor` (`pipeline.rs`) handles legacy flat
//! `Workflow` objects and `|`/`&&`/`,` pipeline composition — this module
//! is for skill-based DAG workflows only.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelState};
use mur_common::pipeline::{PipelineOutput, PipelineStatus, inject_input};
use mur_common::skill::event_log::{RunRecord, record_run};
use mur_common::skill::manifest::{FailureAction, Procedure, ProcedureStep};
use sha2::{Digest, Sha256};
use tokio::time::{Duration, sleep, timeout as tokio_timeout};

/// Appended to every delegated sub-goal so partial execution is declared
/// instead of silent (issue #595).
pub const DELEGATE_REPLY_CONTRACT: &str = "\n\n---\nReply contract: end with a 'Completion:' checklist naming EVERY requested item as done / skipped / blocked. If you run low on turns, deliver partial work and declare the shortfall — never report clean completion over partial execution.";

/// Per-dependency cap on the output text threaded into a dependent step's
/// delegated sub-goal (see `execute_dag`'s dispatch loop). Generous enough to
/// carry a full research/verify reply; bounded so a runaway output can't blow
/// the delegate's context.
const DEP_OUTPUT_EXCERPT_MAX: usize = 24_000;

/// Char-boundary-safe head excerpt of a dependency output.
fn dep_output_excerpt(s: &str) -> String {
    if s.len() <= DEP_OUTPUT_EXCERPT_MAX {
        return s.to_string();
    }
    let mut end = DEP_OUTPUT_EXCERPT_MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…[truncated]", &s[..end])
}

/// Thread completed dependency outputs into a delegated step's sub-goal.
///
/// `depends_on` edges previously only sequenced execution — a delegated step
/// never saw its dependencies' outputs (the worker receives ONLY the message
/// text; channel history is not injected on `channel/delegate`). Appending
/// them to `intent` means e.g. a synthesize step actually receives the
/// research/verify results it depends on. No-op for non-delegated steps,
/// steps without dependencies, or when no dependency has produced output.
fn thread_dep_outputs(
    step: &mut mur_common::skill::manifest::ProcedureStep,
    completed_outputs: &HashMap<String, String>,
) {
    if step.delegate_to.is_none() || step.depends_on.is_empty() {
        return;
    }
    let mut ctx = String::new();
    for dep in &step.depends_on {
        if let Some(out) = completed_outputs.get(dep.as_str()) {
            ctx.push_str(&format!(
                "\n--- output of dependency step {dep} ---\n{}\n",
                dep_output_excerpt(out)
            ));
        }
    }
    if !ctx.is_empty() {
        let base = step
            .intent
            .clone()
            .unwrap_or_else(|| step.description.clone());
        step.intent = Some(format!(
            "{base}\n\n[Outputs from completed dependency steps]{ctx}"
        ));
    }
}

/// Options for a single DAG execution.
pub struct DagExecOptions<'a> {
    /// Piped input from a previous pipeline stage (for `{{input}}` substitution).
    pub input: Option<PipelineOutput>,
    /// `--yes` flag: auto-approve all `needs_approval` steps.
    pub yes: bool,
    /// Explicit override of the env classification (P4-ready).
    pub env_class_override: Option<&'a str>,
    /// Variable substitutions: `(name, value)` pairs for `{{name}}` in commands.
    pub variables: Vec<(String, String)>,
    /// Device identifier for the run ledger.
    pub device_id: String,
    /// Human-readable trigger source: "manual" | "schedule" | "agent".
    pub trigger: &'a str,
    /// Channel the executor runs OVER — events are appended to
    /// `~/.mur/channels/<id>/` as the workflow proceeds (v3a).
    pub channel_id: Option<String>,
    /// Stable id for this logical run. Used to derive deterministic
    /// `idempotency_key`s for channel events. v3b sets keys; v3c enforces dedup,
    /// at which point a crash-rerun MUST reuse the same `run_id`. Empty = none.
    pub run_id: String,
    /// What kind of run this is, for `~/.mur/runs/<run_id>/run.json`. `None`
    /// (or an empty `run_id`) means "do not record" — the legacy path.
    pub run_kind: Option<crate::run_status::RunKind>,
    /// Human-readable label for the run, shown by `mur job list`.
    pub run_label: String,
    /// Cap on the number of steps running concurrently across the whole DAG.
    /// `None` = unbounded (every same-rank step spawned at once — prior
    /// behaviour). `Some(n)` bounds total in-flight steps to `n.max(1)` via a
    /// shared semaphore. The 2026 dynamic-fan-out hard precondition: cap
    /// concurrency, not just cost, or parallel delegations cascade past API
    /// rate limits.
    pub max_concurrency: Option<usize>,
    /// Optional display-only step-lifecycle observer (run-progress UI, Task 3).
    /// Fired `Started` before a step executes and `Done`/`Failed` where its
    /// `StepResult` is recorded. Runs synchronously on executor worker tasks
    /// (ranks execute concurrently via `tokio::spawn`, hence `Send + Sync`):
    /// it MUST be cheap and MUST NOT panic. Purely observational — never
    /// affects control flow. `None` = zero behavior change.
    pub on_step: Option<std::sync::Arc<dyn Fn(StepEvent) + Send + Sync>>,
}

impl DagExecOptions<'_> {
    /// The `run_id` to stamp on the channel events this run writes, so a
    /// rebuild can claim exactly this run's events on a shared, long-lived
    /// channel. `None` for the legacy callers with an empty `run_id` — their
    /// events carry no run_id and are not claimed by any rebuild.
    pub fn event_run_id(&self) -> Option<&str> {
        (!self.run_id.is_empty()).then_some(self.run_id.as_str())
    }
}

impl<'a> Default for DagExecOptions<'a> {
    fn default() -> Self {
        Self {
            input: None,
            yes: false,
            env_class_override: None,
            variables: vec![],
            device_id: "cli".to_string(),
            trigger: "manual",
            channel_id: None,
            run_id: String::new(),
            run_kind: None,
            run_label: String::new(),
            max_concurrency: None,
            on_step: None,
        }
    }
}

/// Step-lifecycle event kind for `DagExecOptions.on_step`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepEventKind {
    Started,
    Done,
    Failed,
}

/// Display-only step lifecycle event for progress observers. The callback
/// runs on executor worker tasks: it MUST be cheap and MUST NOT panic.
#[derive(Debug, Clone)]
pub struct StepEvent {
    pub id: String,
    /// The delegate target agent, when this step is a delegation. `None`
    /// otherwise. ponytail: currently redundant with the progress step's
    /// `worker` (both come from `step.delegate_to`); kept as part of the
    /// generic observer contract. Drop it if no consumer ever reads it.
    #[allow(dead_code)]
    pub agent: Option<String>,
    pub kind: StepEventKind,
    /// Per-step delegate token usage (0 for non-delegate or unknown).
    pub tokens_used: u64,
}

/// Apply one `StepEvent` to the record's `steps` — insert-or-update by step
/// id. `Started` (re)arms a step (a retry emits Started again); `Done`/
/// `Failed` stamp the terminal state. This is the executor's half of "steps
/// must answer what is it doing now" (spec §4): without it the record's
/// steps stay `[]` forever, because nothing else writes them.
fn apply_step_event(record: &mut crate::run_status::RunState, event: &StepEvent) {
    let now = chrono::Utc::now();
    let (state, started_at, ended_at) = match event.kind {
        StepEventKind::Started => (crate::run_status::State::Running, Some(now), None),
        StepEventKind::Done => (crate::run_status::State::Done, None, Some(now)),
        StepEventKind::Failed => (crate::run_status::State::Failed, None, Some(now)),
    };
    if let Some(step) = record.steps.iter_mut().find(|s| s.id == event.id) {
        step.state = state;
        if let Some(ts) = started_at {
            step.started_at = Some(ts);
        }
        if let Some(ts) = ended_at {
            step.ended_at = Some(ts);
        }
        return;
    }
    record.steps.push(crate::run_status::StepState {
        id: event.id.clone(),
        member: event.agent.clone(),
        state,
        started_at,
        ended_at,
    });
}

/// Deterministic idempotency key for a channel event: stable across a
/// crash-rerun of the same logical run, distinct per (channel, run, step, role).
fn idem_key(channel_id: &str, run_id: &str, step_id: &str, suffix: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{channel_id}|{run_id}|{step_id}|{suffix}").as_bytes());
    format!("{:x}", h.finalize())
}

/// Build the `channel/delegate` params for a delegated sub-goal (v3d-2).
///
/// `idempotency_key` is the deterministic `reply_key`: the specialist signs its
/// own reply Message with it so re-dials fold instead of duplicating.
fn build_channel_delegate_params(
    text: &str,
    channel_id: &str,
    child_task_id: &str,
    idempotency_key: &str,
) -> serde_json::Value {
    let text = format!("{}{}", text, DELEGATE_REPLY_CONTRACT);
    serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] },
        "channel_id": channel_id,
        "task_id": child_task_id,
        "idempotency_key": idempotency_key,
    })
}

/// Extract the specialist's reply: the last `role=="agent"` message's joined
/// text parts. Mirrors the Hub's `extract_text` over `task["messages"]`.
fn extract_agent_reply(task: &serde_json::Value) -> String {
    task.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("agent"))
        })
        .and_then(|m| m.get("parts").and_then(|p| p.as_array()))
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// Sum the real token usage a specialist reported in its `Task.usage`
/// (`input_tokens + output_tokens`). 0 when absent — older runtimes, stub
/// backends, or a reply that carried no usage — so accounting degrades to the
/// projection rather than under-counting silently.
fn extract_usage_tokens(task: &serde_json::Value) -> u64 {
    let usage = match task.get("usage") {
        Some(u) => u,
        None => return 0,
    };
    let field = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    field("input_tokens").saturating_add(field("output_tokens"))
}

// ── Graph types ─────────────────────────────────────────────────────────────

#[derive(Debug)]
struct DagNode {
    step: ProcedureStep,
    /// Topological rank (0 = root, N = max ancestors to a root).
    rank: usize,
}

#[derive(Debug)]
#[allow(dead_code)]
struct DagGraph {
    nodes: Vec<DagNode>,
    /// Mapping from step id → index in `nodes`.
    id_to_idx: HashMap<String, usize>,
}

/// Validate a step list (resolvable `depends_on`, no cycles) without executing.
/// Used by fleet router-planning to reject an invalid plan and fall back.
pub(crate) fn validate_steps(steps: &[ProcedureStep]) -> Result<()> {
    build_dag(steps).map(|_| ())
}

/// Build the DAG: assign ids, validate depends_on, detect cycles, compute ranks.
fn build_dag(steps: &[ProcedureStep]) -> Result<DagGraph> {
    // Assign default ids to steps without one.
    let steps: Vec<ProcedureStep> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| ProcedureStep {
            id: s.id.clone().or_else(|| Some(format!("s{i}"))),
            ..s.clone()
        })
        .collect();

    // Build id→index map.
    let id_to_idx: HashMap<String, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.clone().unwrap(), i))
        .collect();

    // Validate depends_on — every referenced id must exist.
    for s in &steps {
        let sid = s.id.as_deref().unwrap_or("");
        for dep in &s.depends_on {
            if !id_to_idx.contains_key(dep) {
                anyhow::bail!(
                    "step `{sid}` depends_on unknown step `{dep}` — available ids: {:?}",
                    id_to_idx.keys().collect::<Vec<_>>()
                );
            }
        }
    }

    // Topo-sort via Kahn: compute in-degree.
    let n = steps.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    for s in steps.iter() {
        let i = id_to_idx[&s.id.clone().unwrap()];
        for dep in &s.depends_on {
            let d = id_to_idx[dep];
            adj[d].push(i);
            in_degree[i] += 1;
        }
    }

    // Kahn: start with all in-degree=0.
    let mut queue: Vec<usize> = (0..n).filter(|i| in_degree[*i] == 0).collect();
    let mut topo = Vec::with_capacity(n);
    while let Some(i) = queue.pop() {
        topo.push(i);
        for &next in &adj[i] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push(next);
            }
        }
    }

    if topo.len() != n {
        // Some steps weren't reachable → cycle.
        let unreachable: Vec<String> = (0..n)
            .filter(|i| !topo.contains(i))
            .map(|i| steps[i].id.clone().unwrap())
            .collect();
        anyhow::bail!(
            "cycle detected in workflow DAG — unreachable steps: {:?}",
            unreachable
        );
    }

    // Assign ranks: rank[i] = 0 + max(rank[dep] + 1) over depends_on.
    let mut rank = vec![0usize; n];
    for &i in &topo {
        for dep in &steps[i].depends_on {
            let d = id_to_idx[dep];
            rank[i] = rank[i].max(rank[d] + 1);
        }
    }

    let nodes: Vec<DagNode> = steps
        .into_iter()
        .enumerate()
        .map(|(i, step)| DagNode {
            step,
            rank: rank[i],
        })
        .collect();

    Ok(DagGraph { nodes, id_to_idx })
}

// ── Step execution ──────────────────────────────────────────────────────────

struct StepResult {
    exit_code: i32,
    output_text: String,
    duration_ms: u64,
    failed_step: Option<String>,
    success: bool,
    /// Real LLM tokens (input + output) this step consumed — non-zero only for
    /// delegate steps, read from the specialist's `Task.usage`. Summed into the
    /// run's `PipelineOutput.tokens_used` for real fleet budget accounting.
    tokens_used: u64,
}

/// Core step execution: run the command, handle approval, return result.
async fn execute_step_inner(
    step: &ProcedureStep,
    opts: &DagExecOptions<'_>,
    step_index: usize,
) -> StepResult {
    let start = std::time::Instant::now();

    // ── Command-mode ──
    if let Some(cmd_template) = &step.command {
        // Variable substitution: {{var_name}} → value
        let mut cmd_string = cmd_template.clone();
        for (name, value) in &opts.variables {
            cmd_string = cmd_string.replace(&format!("{{{{{name}}}}}"), value);
        }
        // Piped input: {{input}}
        let input_text = opts
            .input
            .as_ref()
            .and_then(|o| o.output_text.as_deref())
            .filter(|t| !t.is_empty());
        cmd_string = inject_input(&cmd_string, input_text);

        // Dedent and normalize for display.
        let display_cmd = cmd_string.trim();
        eprintln!(
            "  Step {}: {} (`{}`)",
            step.id.as_deref().unwrap_or(&step_index.to_string()),
            step.description,
            display_cmd
        );

        // Build command with optional timeout.
        let cmd_result = {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c")
                .arg(&cmd_string)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            match step.timeout_secs {
                Some(secs) => match tokio_timeout(Duration::from_secs(secs), cmd.output()).await {
                    Ok(Ok(out)) => Some(out),
                    Ok(Err(e)) => {
                        eprintln!(
                            "  Step {} exec error: {}",
                            step.id.as_deref().unwrap_or(&step_index.to_string()),
                            e
                        );
                        return StepResult {
                            exit_code: 1,
                            output_text: format!("exec error: {e}"),
                            duration_ms: start.elapsed().as_millis() as u64,
                            failed_step: Some(step.description.clone()),
                            success: false,
                            tokens_used: 0,
                        };
                    }
                    Err(_) => {
                        eprintln!(
                            "  Step {} timed out after {secs}s",
                            step.id.as_deref().unwrap_or(&step_index.to_string())
                        );
                        return StepResult {
                            exit_code: -1,
                            output_text: "timeout".to_string(),
                            duration_ms: start.elapsed().as_millis() as u64,
                            failed_step: Some(step.description.clone()),
                            success: false,
                            tokens_used: 0,
                        };
                    }
                },
                None => match cmd.output().await {
                    Ok(out) => Some(out),
                    Err(e) => {
                        eprintln!(
                            "  Step {} exec error: {}",
                            step.id.as_deref().unwrap_or(&step_index.to_string()),
                            e
                        );
                        return StepResult {
                            exit_code: 1,
                            output_text: format!("exec error: {e}"),
                            duration_ms: start.elapsed().as_millis() as u64,
                            failed_step: Some(step.description.clone()),
                            success: false,
                            tokens_used: 0,
                        };
                    }
                },
            }
        };

        let output = cmd_result.unwrap();

        let stderr_str = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
        if !stderr_str.is_empty() {
            eprint!("{}", stderr_str);
        }
        if !stdout_str.is_empty() {
            print!("{}", stdout_str);
        }

        let exit_code = output.status.code().unwrap_or(1);

        // Handle on_failure if exit_code != 0 (caller decides Abort/Skip/Retry).
        StepResult {
            exit_code,
            output_text: stdout_str,
            duration_ms: start.elapsed().as_millis() as u64,
            failed_step: if exit_code != 0 {
                Some(step.description.clone())
            } else {
                None
            },
            success: exit_code == 0,
            tokens_used: 0,
        }
    } else {
        // ── Intent-mode (no command) ──
        eprintln!(
            "  Step {}: {} {}",
            step.id.as_deref().unwrap_or(&step_index.to_string()),
            step.description,
            step.tool
                .as_deref()
                .map(|t| format!("(tool: {t})"))
                .unwrap_or_default()
        );
        StepResult {
            exit_code: 0,
            output_text: step.description.clone(),
            duration_ms: start.elapsed().as_millis() as u64,
            failed_step: None,
            success: true,
            tokens_used: 0,
        }
    }
}

// ── Channel emit helper ─────────────────────────────────────────────────────

use crate::channel_writer::ROUTER_AGENT;

/// Fire-and-forget: open the channel, append one System event SIGNED by the
/// router (`"mur"`, falling back to unsigned when no identity), ignore errors.
fn emit_channel(
    mur_home: &Path,
    channel_id: &str,
    kind: mur_common::channel::EventKind,
    payload: serde_json::Value,
) {
    let _ = mur_channel::ChannelService::open(mur_home).and_then(|svc| {
        crate::channel_writer::append_as_writer(
            &svc,
            mur_home,
            channel_id,
            ROUTER_AGENT,
            mur_common::channel::ChannelActor::System,
            kind,
            payload,
            None,
        )
    });
}

/// Per-step entry-point: wraps `execute_step_inner` with channel ToolCall/ToolResult events.
async fn execute_step(
    step: &ProcedureStep,
    opts: &DagExecOptions<'_>,
    step_index: usize,
    attempt: u32,
    mur_home: &Path,
) -> StepResult {
    let start = std::time::Instant::now();
    let sid = step.id.clone().unwrap_or_else(|| step_index.to_string());
    let observer_agent = step.delegate_to.clone();
    // Display-only step lifecycle emit: MUST be cheap and MUST NOT panic
    // (plain Fn, never `?`'d — see `DagExecOptions.on_step` doc).
    let emit = |kind: StepEventKind, tokens_used: u64| {
        if let Some(cb) = &opts.on_step {
            cb(StepEvent {
                id: sid.clone(),
                agent: observer_agent.clone(),
                kind,
                tokens_used,
            });
        }
    };
    emit(StepEventKind::Started, 0);
    // Retries reuse (run_id, step_id); without an attempt discriminator a
    // succeeding retry's events collide with the failed attempt's idem keys and
    // are dropped by the dedup-aware writer. attempt 0 keeps the original keys
    // (back-compat with already-recorded events + the resume cursor).
    let key = |base: &str| {
        if attempt == 0 {
            base.to_string()
        } else {
            format!("{base}#a{attempt}")
        }
    };

    // ── Resume cursor (v3c): skip steps whose ToolResult is already recorded ──
    if let Some(cid) = opts.channel_id.as_deref() {
        let result_key = idem_key(cid, &opts.run_id, &sid, &key("result"));
        if let Ok(svc) = ChannelService::open(mur_home)
            && let Ok(evs) = svc.load_events(cid)
            && evs.iter().any(|e| {
                e.kind == mur_common::channel::EventKind::ToolResult
                    && e.idempotency_key.as_deref() == Some(result_key.as_str())
                    && e.payload.get("success").and_then(|v| v.as_bool()) == Some(true)
            })
        {
            eprintln!("  Step {sid}: already completed (resume) — skipping");
            emit(StepEventKind::Done, 0);
            return StepResult {
                exit_code: 0,
                output_text: String::new(),
                duration_ms: 0,
                failed_step: None,
                success: true,
                tokens_used: 0,
            };
        }
    }

    // ── Delegation (v3d-2): dial a specialist via `channel/delegate`; the
    // specialist runs the turn AND writes+signs its own reply Message ──
    if let (Some(target), Some(cid)) = (step.delegate_to.as_deref(), opts.channel_id.as_deref()) {
        let start = std::time::Instant::now();
        let canonical = crate::a2a_dial::canonicalize_agent_name(mur_home, target);
        let child_task_id = format!("ct-{}", uuid::Uuid::now_v7());
        let deleg_key = idem_key(cid, &opts.run_id, &sid, &key("delegate"));
        let reply_key = idem_key(cid, &opts.run_id, &sid, &key("reply"));

        // Sub-goal text: explicit intent, else the step description.
        let goal_text = step
            .intent
            .clone()
            .unwrap_or_else(|| step.description.clone());

        // Record the delegation up front (System actor, deterministic key),
        // SIGNED by the router (v3d) via the single-sourced payload builder.
        // The goal snippet lets observers (fleet rail / followed-channel
        // milestones) show WHAT was delegated; clipped so one long step
        // description cannot bloat the append-only log.
        if let Ok(svc) = ChannelService::open(mur_home) {
            const DELEGATION_GOAL_SNIP: usize = 200;
            let goal_snip: String = goal_text.chars().take(DELEGATION_GOAL_SNIP).collect();
            let payload = mur_channel::service::delegation_payload(
                cid,
                &canonical,
                &child_task_id,
                Some(&goal_snip),
                opts.event_run_id(),
            );
            let _ = crate::channel_writer::append_as_writer(
                &svc,
                mur_home,
                cid,
                ROUTER_AGENT,
                ChannelActor::System,
                mur_common::channel::EventKind::Delegation,
                payload,
                Some(deleg_key),
            );
        }
        eprintln!("  Step {sid}: delegate → {canonical}: {goal_text}");

        // v3d-2 (A2 "peer-writes-own"): delegate via the runtime's
        // `channel/delegate` method. The specialist runs the turn AND appends
        // its OWN signed `Agent{self}` reply Message to the channel, signed with
        // the deterministic `reply_key` as idempotency key. We no longer append
        // the reply on the router's behalf.
        //
        // NOTE: `dial_method` is non-streaming, so the v3c streaming HITL relay
        // (the on_hitl mirror closure) is intentionally dropped here. If the
        // specialist gates, it appends its own HitlRequest mirror; a lost
        // *interactive* streaming relay is acceptable for v3d-2 (FLAGGED).
        let params = build_channel_delegate_params(&goal_text, cid, &child_task_id, &reply_key);
        let dial = crate::a2a_dial::dial_method(
            mur_home,
            &canonical,
            "channel/delegate",
            params,
            crate::a2a_dial::DialMode::RequireRunning,
        );

        let result = match dial {
            Ok(task) => {
                // Reply text is extracted ONLY to fill StepResult.output_text —
                // the specialist already wrote+signed the reply Message itself.
                let reply = extract_agent_reply(&task);
                let empty = reply.trim().is_empty();
                StepResult {
                    exit_code: if empty { 1 } else { 0 },
                    output_text: reply,
                    duration_ms: start.elapsed().as_millis() as u64,
                    failed_step: if empty {
                        Some(step.description.clone())
                    } else {
                        None
                    },
                    success: !empty,
                    // Real tokens the specialist's turn consumed, from Task.usage.
                    tokens_used: extract_usage_tokens(&task),
                }
            }
            Err(e) => {
                // Nothing partial is attributed; record a failure Note + fail the
                // step so the DAG's on_failure (Abort/Skip/Retry) decides.
                if let Ok(svc) = ChannelService::open(mur_home) {
                    let _ = crate::channel_writer::append_as_writer(
                        &svc,
                        mur_home,
                        cid,
                        ROUTER_AGENT,
                        ChannelActor::System,
                        mur_common::channel::EventKind::Note,
                        serde_json::json!({ "text": format!("delegate to {canonical} failed: {e:#}") }),
                        None,
                    );
                }
                eprintln!("  Step {sid}: delegate to {canonical} failed: {e:#}");
                StepResult {
                    exit_code: 1,
                    output_text: format!("delegate failed: {e}"),
                    duration_ms: start.elapsed().as_millis() as u64,
                    failed_step: Some(step.description.clone()),
                    success: false,
                    tokens_used: 0,
                }
            }
        };
        emit(
            if result.success {
                StepEventKind::Done
            } else {
                StepEventKind::Failed
            },
            result.tokens_used,
        );
        return result;
    }

    // ── Risk-tiered HITL gate (v3c) ──
    let tier = step.risk.or(if step.needs_approval {
        Some(mur_common::hitl::RiskTier::Destructive)
    } else {
        None
    });
    if let (Some(tier), Some(cid)) = (tier, opts.channel_id.as_deref()) {
        let input = serde_json::json!({
            "command": step.command,
            "intent": step.intent,
            "description": step.description,
        });
        let req = crate::hitl::gate::ActionRequest {
            tier,
            tool_name: step
                .command
                .clone()
                .map(|_| "sh".into())
                .unwrap_or_else(|| "intent".into()),
            tool_input: input.clone(),
            step_or_call_id: sid.clone(),
            agent_id: "mur".into(),
            summary: step.description.clone(),
        };
        let decision = crate::hitl::gate::gate(
            mur_home,
            cid,
            &req,
            opts.yes,
            None,
            Some(opts.run_id.as_str()),
        )
        .await
        .unwrap_or(crate::hitl::gate::GateDecision {
            allow: false,
            reason: "gate error".into(),
            action_hash: String::new(),
        });
        if !decision.allow {
            eprintln!("  Step {sid}: gate denied ({})", decision.reason);
            emit(StepEventKind::Failed, 0);
            return StepResult {
                exit_code: 1,
                output_text: format!("hitl: {}", decision.reason),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: Some(step.description.clone()),
                success: false,
                tokens_used: 0,
            };
        }
        // Re-verify the pin at the execute boundary (fail-closed on drift).
        let now_hash = crate::hitl::pin::action_hash("sh", &input, cid, &sid, "mur");
        if !decision.action_hash.is_empty() && now_hash != decision.action_hash {
            eprintln!("  Step {sid}: hitl_drift at execute boundary — refusing");
            emit(StepEventKind::Failed, 0);
            return StepResult {
                exit_code: 1,
                output_text: "hitl_drift".into(),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: Some(step.description.clone()),
                success: false,
                tokens_used: 0,
            };
        }
    } else if step.needs_approval {
        // No channel: legacy TTY/--yes approval (unchanged behavior).
        let approved = if opts.yes {
            true
        } else if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            #[cfg(feature = "cli")]
            {
                dialoguer::Confirm::new()
                    .with_prompt(format!("Step {sid}: «{}» — run?", step.description))
                    .default(true)
                    .interact()
                    .unwrap_or(false)
            }
            #[cfg(not(feature = "cli"))]
            false
        } else {
            false
        };
        if !approved {
            eprintln!(
                "  Step {sid}: needs_approval, skipped (yields true) — use `--yes` to auto-approve"
            );
            emit(StepEventKind::Done, 0);
            return StepResult {
                exit_code: 0,
                output_text: String::new(),
                duration_ms: 0,
                failed_step: None,
                success: true,
                tokens_used: 0,
            };
        }
    }

    // Guard ToolCall against spurious emit on delegation steps (belt+suspenders
    // in case delegate_to is set but channel_id is None — the branch above
    // handles the channel case; this ensures local runs stay clean too).
    if let (Some(cid), true) = (opts.channel_id.as_deref(), step.delegate_to.is_none()) {
        emit_channel(
            mur_home,
            cid,
            mur_common::channel::EventKind::ToolCall,
            serde_json::json!({
                "step_id": sid,
                "description": step.description,
                "command": step.command,
                "tool": step.tool,
            }),
        );
    }

    let result = execute_step_inner(step, opts, step_index).await;

    if let Some(cid) = opts.channel_id.as_deref() {
        let mut excerpt = result.output_text.clone();
        excerpt.truncate(2048);
        // Use the deterministic idem_key so the resume cursor can match this row.
        let result_key = idem_key(cid, &opts.run_id, &sid, &key("result"));
        let _ = ChannelService::open(mur_home).and_then(|svc| {
            crate::channel_writer::append_as_writer(
                &svc,
                mur_home,
                cid,
                ROUTER_AGENT,
                mur_common::channel::ChannelActor::System,
                mur_common::channel::EventKind::ToolResult,
                serde_json::json!({
                    "step_id": sid,
                    "exit_code": result.exit_code,
                    "success": result.success,
                    "output": excerpt,
                }),
                Some(result_key),
            )
        });
    }

    emit(
        if result.success {
            StepEventKind::Done
        } else {
            StepEventKind::Failed
        },
        result.tokens_used,
    );

    result
}

// ── Core DAG executor ───────────────────────────────────────────────────────

/// Execute a Procedure (skill workflow) DAG. Uses the `--yes` and `variables`
/// from `DagExecOptions`. Records every step outcome via the run-ledger.
///
/// Returns a `PipelineOutput` for composability with the existing pipeline
/// infrastructure (e.g. piping output to the next CLI command).
pub async fn execute_dag(
    mur_home: &Path,
    skill_name: &str,
    procedure: &Procedure,
    opts: &DagExecOptions<'_>,
) -> Result<PipelineOutput> {
    let start = std::time::Instant::now();

    let mut graph = build_dag(&procedure.steps)?;

    // Emit start StateChange (Working) if running over a channel. `first_seq`
    // — the seq this run's first event will land on — is computed BEFORE the
    // transition fires, so the sidecar can bound the rebuild to exactly this
    // run's events. Full load is O(channel size) once per run start, which
    // is acceptable: there is no seq-cursor API, and the load is the only
    // way to learn the next seq.
    let mut first_seq: Option<u64> = None;
    if let Some(cid) = opts.channel_id.as_deref()
        && let Ok(svc) = ChannelService::open(mur_home)
    {
        first_seq = match svc.load_events(cid) {
            Ok(events) => Some(events.last().map(|e| e.seq + 1).unwrap_or(0)),
            Err(_) => None,
        };
        let _ = svc.transition(
            cid,
            ChannelState::Working,
            ChannelActor::System,
            opts.event_run_id(),
        );
    }

    if graph.nodes.is_empty() {
        return Ok(PipelineOutput {
            workflow_id: skill_name.to_string(),
            status: PipelineStatus::Success,
            output_text: Some(String::new()),
            output_data: None,
            exit_code: 0,
            duration_ms: 0,
            tokens_used: 0,
        });
    }

    // Record the run so it can be queried while it executes. A run is only
    // recorded when it has both an id and a kind; the legacy callers that
    // pass neither behave exactly as before.
    let recorded = (!opts.run_id.is_empty()).then_some(opts.run_kind).flatten();
    let mut heartbeat = if let Some(kind) = recorded {
        let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
        let now = chrono::Utc::now();
        let record = crate::run_status::RunState {
            schema: crate::run_status::RUN_SCHEMA,
            run_id: opts.run_id.clone(),
            channel_id: opts.channel_id.clone(),
            kind,
            label: opts.run_label.clone(),
            pid: std::process::id(),
            started_at: now,
            last_heartbeat_at: Some(now),
            state: crate::run_status::State::Running,
            steps: vec![],
            blocked_on: None,
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            build_sha: mur_common::build::SHORT_SHA.to_string(),
        };
        // Bookkeeping must never take down real work: a run record that can't
        // be written (full disk, unwritable ~/.mur/runs/, ...) should not
        // fail the run before a single step executes. Log and proceed with
        // no record and no heartbeat ticker — there is nothing for it to
        // beat against.
        match crate::run_status::store::save(mur_home, &record) {
            Ok(()) => {
                // The rebuild index travels in its own sidecar so a corrupt
                // run.json cannot take the rebuild path down with it. Every
                // sidecar field is a fact known here at recording time —
                // nothing inferred.
                if let Some(cid) = opts.channel_id.as_deref() {
                    match first_seq {
                        Some(seq) => {
                            let sidecar = crate::run_status::Sidecar {
                                schema: crate::run_status::SIDECAR_SCHEMA,
                                channel_id: cid.to_string(),
                                kind,
                                first_seq: seq,
                            };
                            if let Err(error) = crate::run_status::store::save_sidecar(
                                mur_home,
                                &opts.run_id,
                                &sidecar,
                            ) {
                                // Not silent: a lost sidecar quietly disables
                                // the rebuild this run's channel could later
                                // provide, so say so.
                                tracing::warn!(
                                    run_id = %opts.run_id,
                                    %error,
                                    "failed to record the run sidecar; \
                                     rebuilding this run from its channel is disabled"
                                );
                            }
                        }
                        None => {
                            tracing::warn!(
                                run_id = %opts.run_id,
                                "cannot compute the run's first channel seq; \
                                 sidecar not written"
                            );
                        }
                    }
                }
                Some(crate::run_status::heartbeat::Heartbeat::spawn(
                    mur_home.to_path_buf(),
                    opts.run_id.clone(),
                    std::time::Duration::from_secs(cfg.runs.heartbeat_interval_secs),
                ))
            }
            Err(error) => {
                tracing::warn!(
                    run_id = %opts.run_id,
                    %error,
                    "failed to record run status; continuing without run tracking"
                );
                None
            }
        }
    } else {
        None
    };

    // Live step progress (spec §4): while a run is recorded, mirror every
    // `StepEvent` into the record's `steps` through an internal observer, so
    // `mur job status` / `mur_job_status` can answer "what is it doing now?"
    // instead of showing an empty list. One locked `store::update` per event
    // is the whole budget — the observer contract says MUST be cheap and
    // MUST NOT panic, so a failed write is warned once and dropped;
    // bookkeeping must never take down the run it observes. The caller's
    // observer is wrapped, not replaced: it still fires exactly as before,
    // AFTER the record update so a progress renderer that reads the record
    // sees the just-applied step.
    let internal_on_step: Option<std::sync::Arc<dyn Fn(StepEvent) + Send + Sync>> =
        if recorded.is_some() {
            let home = mur_home.to_path_buf();
            let rid = opts.run_id.clone();
            let warn_once = Arc::new(std::sync::Once::new());
            Some(Arc::new(move |event: StepEvent| {
                if let Err(error) = crate::run_status::store::update(&home, &rid, |record| {
                    apply_step_event(record, &event);
                }) {
                    warn_once.call_once(|| {
                        tracing::warn!(
                            run_id = %rid,
                            %error,
                            "failed to record a step event; `mur job status` steps \
                             may lag behind the run"
                        );
                    });
                }
            }))
        } else {
            None
        };
    let composed_on_step: Option<std::sync::Arc<dyn Fn(StepEvent) + Send + Sync>> =
        match (opts.on_step.clone(), internal_on_step) {
            (Some(caller), Some(internal)) => Some(Arc::new(move |e: StepEvent| {
                internal(e.clone());
                caller(e);
            })),
            (Some(caller), None) => Some(caller),
            (None, Some(internal)) => Some(internal),
            (None, None) => None,
        };

    // Closure for terminal StateChange — call before each PipelineOutput return.
    let emit_final = |failed: bool| {
        if let Some(cid) = opts.channel_id.as_deref() {
            let st = if failed {
                ChannelState::Failed
            } else {
                ChannelState::Completed
            };
            let _ = ChannelService::open(mur_home)
                .and_then(|svc| svc.transition(cid, st, ChannelActor::System, opts.event_run_id()));
        }
    };

    // Group by rank.
    let max_rank = graph.nodes.iter().map(|n| n.rank).max().unwrap_or(0);
    let mut overall_exit_code = 0i32;
    let mut overall_output = String::new();
    // Real LLM tokens summed across every step (delegate turns report usage;
    // others contribute 0) → the run's PipelineOutput.tokens_used.
    let mut overall_tokens: u64 = 0;

    // Optional global concurrency cap. One semaphore for the whole run bounds
    // total in-flight steps (across all ranks). `None` => no permit, unbounded.
    let sem = opts
        .max_concurrency
        .map(|n| std::sync::Arc::new(tokio::sync::Semaphore::new(n.max(1))));

    // step id → output_text of successfully completed steps, so later ranks
    // can thread dependency outputs into their delegated sub-goals.
    let mut completed_outputs: HashMap<String, String> = HashMap::new();

    for rank in 0..=max_rank {
        let indices: Vec<usize> = (0..graph.nodes.len())
            .filter(|i| graph.nodes[*i].rank == rank)
            .collect();

        if indices.is_empty() {
            continue;
        }

        // Concurrent: spawn each step in this rank.
        // Extract owned values from opts for the spawned tasks.
        let opt_yes = opts.yes;
        let opt_input = opts.input.clone();
        let opt_env_override = opts.env_class_override.map(|s| s.to_string());
        let opt_vars = opts.variables.clone();
        let opt_dev_id = opts.device_id.clone();
        let opt_trigger = opts.trigger.to_string();
        let opt_chan_id = opts.channel_id.clone();
        let opt_run_id = opts.run_id.clone();
        let opt_on_step = composed_on_step.clone();
        let mut handles = Vec::new();
        for &i in &indices {
            // Mutating the graph node (not the local clone) keeps retries
            // consistent with the augmented sub-goal.
            thread_dep_outputs(&mut graph.nodes[i].step, &completed_outputs);
            let step = graph.nodes[i].step.clone();
            let env_override = opt_env_override.clone();
            let dev_id = opt_dev_id.clone();
            let inp = opt_input.clone();
            let vars = opt_vars.clone();
            let tr = opt_trigger.clone();
            let chan_id = opt_chan_id.clone();
            let run_id = opt_run_id.clone();
            let on_step = opt_on_step.clone();
            let sem = sem.clone();
            let mh = mur_home.to_path_buf();
            handles.push(tokio::task::spawn(async move {
                // Hold a permit for the whole step when a cap is set.
                let _permit = match sem {
                    Some(s) => Some(s.acquire_owned().await.expect("semaphore open")),
                    None => None,
                };
                let opts_clone = DagExecOptions {
                    yes: opt_yes,
                    input: inp,
                    env_class_override: env_override.as_deref(),
                    variables: vars,
                    device_id: dev_id,
                    trigger: &tr,
                    channel_id: chan_id,
                    run_id,
                    // Per-step sub-options, not a run of their own: this
                    // clone drives one `execute_step` call inside the rank
                    // loop, not a recursive `execute_dag`, so it never reads
                    // `run_kind`/`run_label`. Neutralized the same way
                    // `max_concurrency` already is on this line.
                    run_kind: None,
                    run_label: String::new(),
                    max_concurrency: None,
                    on_step,
                };
                execute_step(&step, &opts_clone, i, 0, &mh).await
            }));
        }

        let mut results = Vec::with_capacity(indices.len());
        for h in handles {
            match h.await {
                Ok(r) => results.push(r),
                Err(e) => {
                    eprintln!("  ⚠ Task join error: {}", e);
                    results.push(StepResult {
                        exit_code: 1,
                        output_text: format!("task join error: {e}"),
                        duration_ms: 0,
                        failed_step: Some("(task)".to_string()),
                        success: false,
                        tokens_used: 0,
                    });
                }
            }
        }

        // Collect results and record to ledger.
        for ri in 0..results.len() {
            let result = &results[ri];
            let step = &graph.nodes[indices[ri]].step;
            overall_tokens = overall_tokens.saturating_add(result.tokens_used);

            // Write run-ledger record.
            let stderr_for_ledger = if !result.success && result.output_text.is_empty() {
                Some(result.output_text.as_str())
            } else {
                None
            };
            record_run(
                mur_home,
                skill_name,
                &opts.device_id,
                &RunRecord {
                    success: result.success,
                    duration_ms: Some(result.duration_ms),
                    exit_code: Some(result.exit_code),
                    stderr: stderr_for_ledger,
                    failed_step: result.failed_step.clone(),
                    trigger: opts.trigger,
                    env_class_override: opts.env_class_override,
                },
            )
            .ok();

            if !result.output_text.is_empty() {
                if !overall_output.is_empty() {
                    overall_output.push('\n');
                }
                overall_output.push_str(&result.output_text);
            }

            if result.exit_code != 0 {
                overall_exit_code = result.exit_code;

                // Handle on_failure strategy.
                let sid = step.id.as_deref().unwrap_or("?");
                match step.on_failure {
                    FailureAction::Abort => {
                        eprintln!(
                            "  Step {sid} failed (exit {}), aborting workflow",
                            result.exit_code
                        );
                        emit_final(true);
                        finalize_run(
                            mur_home,
                            &opts.run_id,
                            recorded.is_some(),
                            &mut heartbeat,
                            true,
                        )
                        .await;
                        return Ok(PipelineOutput {
                            workflow_id: skill_name.to_string(),
                            status: PipelineStatus::Failed,
                            output_text: Some(overall_output),
                            output_data: None,
                            exit_code: result.exit_code,
                            duration_ms: start.elapsed().as_millis() as u64,
                            tokens_used: overall_tokens,
                        });
                    }
                    FailureAction::Skip => {
                        eprintln!("  Step {sid} failed (exit {}), skipping", result.exit_code);
                        overall_exit_code = 0;
                    }
                    FailureAction::Retry => {
                        let max_retries = step.retry.as_ref().map(|r| r.max_retries).unwrap_or(1);
                        let backoff = step
                            .retry
                            .as_ref()
                            .and_then(|r| r.backoff_secs)
                            .unwrap_or(0);
                        for attempt in 0..max_retries {
                            if backoff > 0 {
                                sleep(Duration::from_secs(backoff as u64)).await;
                            }
                            eprintln!(
                                "  Step {sid} failed, retry {}/{}...",
                                attempt + 1,
                                max_retries
                            );
                            let retry_result =
                                execute_step(step, opts, indices[ri], attempt + 1, mur_home).await;
                            // Each retry is additional real spend (the original
                            // attempt was already counted once at the per-step
                            // accumulation); count every attempt so the budget
                            // guard never under-counts a retried delegate.
                            overall_tokens =
                                overall_tokens.saturating_add(retry_result.tokens_used);
                            if retry_result.success {
                                results[ri] = retry_result;
                                overall_exit_code = 0;
                                break;
                            } else if attempt + 1 == max_retries {
                                let _retry_code = retry_result.exit_code;
                                eprintln!("  Step {sid} retry exhausted, aborting workflow");
                                emit_final(true);
                                finalize_run(
                                    mur_home,
                                    &opts.run_id,
                                    recorded.is_some(),
                                    &mut heartbeat,
                                    true,
                                )
                                .await;
                                return Ok(PipelineOutput {
                                    workflow_id: skill_name.to_string(),
                                    status: PipelineStatus::Failed,
                                    output_text: Some(overall_output),
                                    output_data: None,
                                    exit_code: retry_result.exit_code,
                                    duration_ms: start.elapsed().as_millis() as u64,
                                    tokens_used: overall_tokens,
                                });
                            }
                        }
                    }
                }
            }

            // Record the step's final output (post-retry) so later ranks can
            // thread it into dependent delegated sub-goals.
            let final_result = &results[ri];
            if final_result.success
                && !final_result.output_text.is_empty()
                && let Some(id) = step.id.as_deref()
            {
                completed_outputs.insert(id.to_string(), final_result.output_text.clone());
            }
        }
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    let status = if overall_exit_code == 0 {
        PipelineStatus::Success
    } else {
        PipelineStatus::Failed
    };

    emit_final(overall_exit_code != 0);
    finalize_run(
        mur_home,
        &opts.run_id,
        recorded.is_some(),
        &mut heartbeat,
        overall_exit_code != 0,
    )
    .await;
    Ok(PipelineOutput {
        workflow_id: skill_name.to_string(),
        status,
        output_text: Some(overall_output),
        output_data: None,
        exit_code: overall_exit_code,
        duration_ms,
        tokens_used: overall_tokens,
    })
}

/// Stop the run's heartbeat and stamp its terminal state.
///
/// The stop MUST be awaited before the terminal save. `Heartbeat::stop` is
/// async because flipping its flag is not enough: a beat already inside
/// `beat_once` has passed the flag check, and its read-modify-write would
/// clobber the terminal state back to `running` with a fresh heartbeat — a
/// finished run reported alive forever, which is the exact failure this
/// module exists to prevent. Awaiting guarantees any in-flight beat lands
/// BEFORE this save, so the terminal write wins.
///
/// Mirrors `emit_final`: call it before every `PipelineOutput` return that
/// can be reached once a run has been recorded.
async fn finalize_run(
    mur_home: &std::path::Path,
    run_id: &str,
    recorded: bool,
    heartbeat: &mut Option<crate::run_status::heartbeat::Heartbeat>,
    failed: bool,
) {
    if !recorded {
        return;
    }
    if let Some(hb) = heartbeat.take() {
        hb.stop().await;
    }
    // `update` holds an exclusive lock across load-modify-save. A bare
    // load/save pair here would race `mur job stop` in another process, which
    // does the same read-modify-write on the same file.
    let _ = crate::run_status::store::update(mur_home, run_id, |record| {
        record.state = if failed {
            crate::run_status::State::Failed
        } else {
            crate::run_status::State::Done
        };
    });
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::manifest::ProcedureStep;

    fn step(id: &str, deps: &[&str], cmd: Option<&str>) -> ProcedureStep {
        ProcedureStep {
            id: Some(id.to_string()),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            command: cmd.map(|s| s.to_string()),
            description: format!("step {id}"),
            ..Default::default()
        }
    }

    #[test]
    fn idem_key_is_deterministic_and_distinct() {
        let a = idem_key("chan", "run", "s0", "delegate");
        let b = idem_key("chan", "run", "s0", "delegate");
        let c = idem_key("chan", "run", "s0", "reply");
        assert_eq!(a, b, "same inputs → same key (crash-rerun stable)");
        assert_ne!(a, c, "different suffix → different key");
        assert_eq!(a.len(), 64, "sha256 hex");
    }

    #[test]
    fn channel_delegate_params_thread_goal_channel_task_and_idem_key() {
        // v3d-2: the concierge delegates via `channel/delegate`, threading the
        // channel id + the deterministic reply_key (as idempotency_key) so the
        // specialist signs its OWN reply Message and re-dials fold.
        let p = build_channel_delegate_params("find the bug", "chan-1", "child-1", "rk-deadbeef");
        assert_eq!(p["channel_id"], "chan-1");
        assert_eq!(p["task_id"], "child-1");
        assert_eq!(p["idempotency_key"], "rk-deadbeef");
        assert_eq!(p["message"]["role"], "user");
        let text = p["message"]["parts"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("find the bug"));
        assert!(text.contains("Completion:"));
    }

    #[test]
    fn thread_dep_outputs_appends_completed_dependency_outputs() {
        let mut s = step("s3", &["s1", "s2"], None);
        s.delegate_to = Some("dr_worker_3".into());
        s.intent = Some("synthesize the brief".into());
        let outputs: HashMap<String, String> = [
            ("s1".to_string(), "claims + citations".to_string()),
            ("s2".to_string(), "CONFIRM x3".to_string()),
        ]
        .into();
        thread_dep_outputs(&mut s, &outputs);
        let intent = s.intent.unwrap();
        assert!(intent.starts_with("synthesize the brief"));
        assert!(intent.contains("[Outputs from completed dependency steps]"));
        assert!(intent.contains("--- output of dependency step s1 ---\nclaims + citations"));
        assert!(intent.contains("--- output of dependency step s2 ---\nCONFIRM x3"));
    }

    #[test]
    fn thread_dep_outputs_noop_without_delegate_or_deps_or_outputs() {
        // Non-delegated step: untouched even with completed deps.
        let mut cmd_step = step("s1", &["s0"], Some("echo hi"));
        let outputs: HashMap<String, String> = [("s0".to_string(), "out".to_string())].into();
        thread_dep_outputs(&mut cmd_step, &outputs);
        assert!(cmd_step.intent.is_none());
        // Delegated step whose deps produced nothing: intent unchanged.
        let mut d = step("s2", &["s9"], None);
        d.delegate_to = Some("w".into());
        d.intent = Some("go".into());
        thread_dep_outputs(&mut d, &outputs);
        assert_eq!(d.intent.as_deref(), Some("go"));
    }

    #[test]
    fn dep_output_excerpt_truncates_on_char_boundary() {
        let s = "研".repeat(DEP_OUTPUT_EXCERPT_MAX); // 3 bytes per char
        let e = dep_output_excerpt(&s);
        assert!(e.len() <= DEP_OUTPUT_EXCERPT_MAX + "\n…[truncated]".len());
        assert!(e.ends_with("…[truncated]"));
        // Short input passes through untouched.
        assert_eq!(dep_output_excerpt("ok"), "ok");
    }

    #[test]
    fn extract_agent_reply_takes_last_agent_message() {
        let task = serde_json::json!({
            "id": "t1",
            "messages": [
                {"role":"user","parts":[{"kind":"text","text":"q"}]},
                {"role":"agent","parts":[{"kind":"text","text":"partial "},{"kind":"text","text":"answer"}]}
            ]
        });
        assert_eq!(extract_agent_reply(&task), "partial answer");
        // No agent message → empty.
        let empty = serde_json::json!({ "messages": [{"role":"user","parts":[]}] });
        assert_eq!(extract_agent_reply(&empty), "");
    }

    #[test]
    fn linear_chain_toposorts() {
        let steps = vec![
            step("s0", &[], Some("echo zero")),
            step("s1", &["s0"], Some("echo one")),
            step("s2", &["s1"], Some("echo two")),
        ];
        let graph = build_dag(&steps).unwrap();
        assert_eq!(graph.nodes.len(), 3);
        // s0 rank 0, s1 rank 1, s2 rank 2
        for n in &graph.nodes {
            let id = n.step.id.as_deref().unwrap();
            let expected = match id {
                "s0" => 0,
                "s1" => 1,
                "s2" => 2,
                _ => unreachable!(),
            };
            assert_eq!(n.rank, expected, "step {id} expected rank {expected}");
        }
    }

    #[test]
    fn concurrent_roots() {
        let steps = vec![
            step("s0", &[], None), // root
            step("s1", &[], None), // root
            step("s2", &["s0", "s1"], None),
        ];
        let graph = build_dag(&steps).unwrap();
        for n in &graph.nodes {
            match n.step.id.as_deref().unwrap() {
                "s0" | "s1" => assert_eq!(n.rank, 0),
                "s2" => assert_eq!(n.rank, 1),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn cycle_detected() {
        let steps = vec![step("s0", &["s1"], None), step("s1", &["s0"], None)];
        let err = build_dag(&steps).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cycle"), "expected cycle error, got: {msg}");
    }

    #[test]
    fn unknown_dep_detected() {
        let steps = vec![step("s0", &["nonexistent"], None)];
        let err = build_dag(&steps).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("nonexistent"),
            "expected nonexistent dep error, got: {msg}"
        );
    }

    #[test]
    fn missing_ids_autogenerated() {
        let steps = vec![ProcedureStep {
            id: None,
            depends_on: vec![],
            command: Some("echo hi".to_string()),
            ..Default::default()
        }];
        let graph = build_dag(&steps).unwrap();
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].step.id.as_deref(), Some("s0"));
    }

    #[test]
    fn empty_steps_returns_ok() {
        let tmp = tempfile::TempDir::new().unwrap();
        let proc = Procedure {
            variables: vec![],
            steps: vec![],
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt
            .block_on(execute_dag(
                tmp.path(),
                "empty-test",
                &proc,
                &DagExecOptions::default(),
            ))
            .unwrap();
        assert_eq!(out.exit_code, 0);
    }

    // channel_run_refuses_needs_approval removed: v3c gates via hitl::gate instead
    // of refusing. See high_risk_step_gates_and_runs_when_preapproved below.

    #[tokio::test]
    async fn on_step_observer_sees_start_and_done() {
        use std::sync::{Arc, Mutex};

        let proc = Procedure {
            variables: vec![],
            steps: vec![
                step("s1", &[], Some("echo one")),
                step("s2", &[], Some("echo two")),
            ],
        };
        let seen: Arc<Mutex<Vec<(String, StepEventKind)>>> = Arc::new(Mutex::new(vec![]));
        let sink = seen.clone();
        let opts = DagExecOptions {
            on_step: Some(Arc::new(move |e: StepEvent| {
                sink.lock().unwrap().push((e.id, e.kind));
            })),
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let _ = execute_dag(tmp.path(), "test", &proc, &opts).await.unwrap();
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&("s1".into(), StepEventKind::Started)));
        assert!(seen.contains(&("s1".into(), StepEventKind::Done)));
        assert!(seen.contains(&("s2".into(), StepEventKind::Done)));
    }

    #[tokio::test]
    async fn resume_skips_a_step_already_completed() {
        use mur_channel::ChannelService;
        use mur_common::channel::EventKind;

        let tmp = tempfile::TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("resume-wf").unwrap();

        let proc = Procedure {
            variables: vec![],
            steps: vec![
                step("s0", &[], Some("echo zero")),
                step("s1", &["s0"], Some("echo one")),
            ],
        };
        let opts = DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "run-1".into(),
            yes: true,
            ..Default::default()
        };
        execute_dag(tmp.path(), "resume-wf", &proc, &opts)
            .await
            .unwrap();
        let after_first = svc.load_events(&ch.id).unwrap().len();

        execute_dag(tmp.path(), "resume-wf", &proc, &opts)
            .await
            .unwrap();
        let tr_after_second = svc
            .load_events(&ch.id)
            .unwrap()
            .iter()
            .filter(|e| e.kind == EventKind::ToolResult)
            .count();
        assert_eq!(
            tr_after_second, 2,
            "rerun did not duplicate completed-step results"
        );
        let _ = after_first;
    }

    #[tokio::test]
    async fn high_risk_step_gates_and_runs_when_preapproved() {
        use mur_channel::ChannelService;
        use mur_common::channel::EventKind;
        use mur_common::hitl::RiskTier;

        let tmp = tempfile::TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("gated-wf").unwrap();
        let mut s = step("s0", &[], Some("echo done"));
        s.risk = Some(RiskTier::Destructive);
        let proc = Procedure {
            variables: vec![],
            steps: vec![s],
        };
        let opts = DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "run-1".into(),
            yes: true,
            ..Default::default()
        };
        let out = execute_dag(tmp.path(), "gated-wf", &proc, &opts)
            .await
            .unwrap();
        assert_eq!(out.exit_code, 0);
        let kinds: Vec<_> = svc
            .load_events(&ch.id)
            .unwrap()
            .iter()
            .map(|e| e.kind)
            .collect();
        assert!(
            kinds.contains(&EventKind::HitlRequest),
            "high-risk step raised a gate"
        );
    }

    #[test]
    fn channel_run_emits_attributed_event_trail() {
        use mur_channel::ChannelService;
        use mur_common::channel::{ChannelActor, ChannelState, EventKind};

        let tmp = tempfile::TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("test-skill").unwrap();

        let proc = Procedure {
            variables: vec![],
            steps: vec![ProcedureStep {
                id: Some("s0".to_string()),
                command: Some("echo hi".to_string()),
                description: "echo step".to_string(),
                ..Default::default()
            }],
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(execute_dag(
            tmp.path(),
            "test-skill",
            &proc,
            &DagExecOptions {
                channel_id: Some(ch.id.clone()),
                ..DagExecOptions::default()
            },
        ))
        .unwrap();

        let evs = svc.load_events(&ch.id).unwrap();
        let kinds: Vec<_> = evs.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds.first(),
            Some(&EventKind::StateChange),
            "first event must be StateChange(Working)"
        );
        assert_eq!(
            kinds.last(),
            Some(&EventKind::StateChange),
            "last event must be StateChange(Completed)"
        );
        assert_eq!(
            evs.iter().filter(|e| e.kind == EventKind::ToolCall).count(),
            1,
            "one ToolCall per step"
        );
        assert_eq!(
            evs.iter()
                .filter(|e| e.kind == EventKind::ToolResult)
                .count(),
            1,
            "one ToolResult per step"
        );
        assert!(
            evs.iter().all(|e| e.actor == ChannelActor::System),
            "all events must have actor=System"
        );
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().state,
            ChannelState::Completed
        );
        let tr = evs
            .iter()
            .find(|e| e.kind == EventKind::ToolResult)
            .unwrap();
        assert_eq!(tr.payload["exit_code"], 0);
    }

    #[tokio::test]
    async fn max_concurrency_bounds_parallel_steps() {
        // 6 independent rank-0 steps. Each registers itself in a shared `run/`
        // dir, records how many steps are concurrently registered, sleeps, then
        // deregisters. The MAX recorded count is the observed peak concurrency —
        // a deterministic property of the executor's semaphore, NOT a wall-clock
        // measurement, so it does not flake on slow/loaded CI runners the way the
        // old timing-ratio assertion did (chronically red on Windows/macOS).

        // Build a procedure whose steps probe live concurrency into `probe_dir`.
        // Forward-slash paths so the `sh -c` body works under Git Bash on Windows.
        fn probe_proc(probe_dir: &str) -> Procedure {
            Procedure {
                variables: vec![],
                steps: (0..6)
                    .map(|i| ProcedureStep {
                        description: format!("s{i}"),
                        command: Some(format!(
                            "mkdir -p '{probe_dir}/run'; : > '{probe_dir}/run/{i}'; \
                             ls '{probe_dir}/run' | wc -l >> '{probe_dir}/peaks'; \
                             sleep 0.2; rm -f '{probe_dir}/run/{i}'"
                        )),
                        id: Some(format!("s{i}")),
                        ..Default::default()
                    })
                    .collect(),
            }
        }
        // Highest concurrency the steps observed (max line in `peaks`).
        fn observed_peak(probe_dir: &std::path::Path) -> usize {
            std::fs::read_to_string(probe_dir.join("peaks"))
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.trim().parse::<usize>().ok())
                .max()
                .unwrap_or(0)
        }
        fn fwd(p: &std::path::Path) -> String {
            p.display().to_string().replace('\\', "/")
        }

        // Bounded to 2 -> the executor's semaphore guarantees <= 2 concurrent.
        let tmp_b = tempfile::TempDir::new().unwrap();
        let probe_b = tempfile::TempDir::new().unwrap();
        let opts = DagExecOptions {
            max_concurrency: Some(2),
            ..Default::default()
        };
        execute_dag(
            tmp_b.path(),
            "cc-bounded",
            &probe_proc(&fwd(probe_b.path())),
            &opts,
        )
        .await
        .unwrap();
        let bounded_peak = observed_peak(probe_b.path());

        // Unbounded -> all 6 run in a single wave.
        let tmp_u = tempfile::TempDir::new().unwrap();
        let probe_u = tempfile::TempDir::new().unwrap();
        let opts2 = DagExecOptions {
            max_concurrency: None,
            ..Default::default()
        };
        execute_dag(
            tmp_u.path(),
            "cc-unbounded",
            &probe_proc(&fwd(probe_u.path())),
            &opts2,
        )
        .await
        .unwrap();
        let unbounded_peak = observed_peak(probe_u.path());

        assert!(
            bounded_peak <= 2,
            "bounded peak {bounded_peak} exceeded the max_concurrency cap of 2"
        );
        assert!(
            unbounded_peak >= 3,
            "unbounded peak {unbounded_peak} should exceed the cap (cap not lifted / steps not parallel)"
        );
    }

    /// A run with an id must be observable from disk while it executes, and
    /// must land on a terminal state when it finishes. Without this, a
    /// timeout is the only signal a caller ever gets — which is the defect.
    #[tokio::test]
    async fn execute_dag_records_and_finalizes_a_run() {
        use crate::run_status::store;

        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let procedure = Procedure {
            variables: vec![],
            steps: vec![step("s1", &[], None)],
        };
        let opts = DagExecOptions {
            run_id: "run-under-test".into(),
            run_kind: Some(crate::run_status::RunKind::Workflow),
            run_label: "test run".into(),
            ..Default::default()
        };

        let _ = execute_dag(mur_home, "test-skill", &procedure, &opts).await;

        let run = store::load(mur_home, "run-under-test")
            .unwrap()
            .expect("execute_dag never wrote run.json");
        assert_eq!(run.run_id, "run-under-test");
        assert_eq!(
            run.pid,
            std::process::id(),
            "must record the orchestrator pid"
        );
        assert!(
            run.state.is_terminal(),
            "run left non-terminal after execute_dag returned: {:?}",
            run.state
        );
    }

    /// THE regression for the review's empty-steps finding: the record is
    /// written with `steps: []` and nothing ever updates it, so `mur job
    /// status` cannot answer "what is it doing now?". Drive the real
    /// executor over a one-step procedure and assert the FINAL record's
    /// steps reflect the lifecycle the `on_step` observer saw — Started
    /// stamped, then Done, with both timestamps set. The point is that
    /// steps are no longer empty.
    #[tokio::test]
    async fn recorded_run_steps_reflect_the_step_lifecycle() {
        use crate::run_status::{State, store};

        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let procedure = Procedure {
            variables: vec![],
            steps: vec![step("s1", &[], Some("echo hi"))],
        };
        let opts = DagExecOptions {
            run_id: "run-with-steps".into(),
            run_kind: Some(crate::run_status::RunKind::Workflow),
            run_label: "steps run".into(),
            ..Default::default()
        };

        let _ = execute_dag(mur_home, "test-skill", &procedure, &opts).await;

        let run = store::load(mur_home, "run-with-steps")
            .unwrap()
            .expect("execute_dag never wrote run.json");
        assert_eq!(
            run.steps.len(),
            1,
            "the record's steps were never updated — `mur job status` would \
             show no step rows for a step that ran"
        );
        let step0 = &run.steps[0];
        assert_eq!(step0.id, "s1", "step id must be the DAG step id");
        assert_eq!(
            step0.state,
            State::Done,
            "the final record must show the step done"
        );
        assert!(
            step0.started_at.is_some() && step0.ended_at.is_some(),
            "both lifecycle timestamps must be stamped: {step0:?}"
        );
    }

    /// A run-status recording failure (e.g. an unwritable `~/.mur/runs/`)
    /// must not fail the run itself — observability must never take down
    /// the thing it observes. Force `store::save`'s `create_dir_all` to fail
    /// deterministically by putting a plain file where the run's directory
    /// needs to go, then prove `execute_dag` still runs its step to a
    /// successful conclusion instead of propagating the I/O error.
    #[tokio::test]
    async fn execute_dag_survives_a_run_recording_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let run_id = "run-that-cannot-be-recorded";

        // `store::save` does `create_dir_all(<mur_home>/runs/<run_id>)`; a
        // regular file already sitting at that exact path makes that call
        // fail every time, deterministically, with no timing dependency.
        std::fs::create_dir_all(mur_home.join("runs")).unwrap();
        std::fs::write(mur_home.join("runs").join(run_id), b"not a directory").unwrap();

        let procedure = Procedure {
            variables: vec![],
            steps: vec![step("s1", &[], None)],
        };
        let opts = DagExecOptions {
            run_id: run_id.into(),
            run_kind: Some(crate::run_status::RunKind::Workflow),
            run_label: "test run".into(),
            ..Default::default()
        };

        let output = execute_dag(mur_home, "test-skill", &procedure, &opts)
            .await
            .expect(
                "execute_dag returned Err — a run-status recording failure \
                 propagated out of the executor instead of being logged and \
                 ignored, so bookkeeping took down real work",
            );
        assert_eq!(
            output.status,
            PipelineStatus::Success,
            "execute_dag did not complete its step after a run-status \
             recording failure — bookkeeping is taking down real work"
        );
    }

    /// An empty `run_id` is the legacy default. It must not create a
    /// directory called "" under runs/.
    #[tokio::test]
    async fn execute_dag_without_a_run_id_records_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let procedure = Procedure {
            variables: vec![],
            steps: vec![step("s1", &[], None)],
        };
        let opts = DagExecOptions::default();

        let _ = execute_dag(tmp.path(), "test-skill", &procedure, &opts).await;

        assert!(
            crate::run_status::store::list_ids(tmp.path())
                .unwrap()
                .is_empty(),
            "recorded a run for an empty run_id"
        );
    }

    /// Pin the executor's own `sidecar.json` write — the `save_sidecar` call
    /// in `execute_dag`'s recording block. A store-level round-trip test
    /// would pass even if the executor never called it, and then a corrupt
    /// run.json would silently hide the run: exactly the defect the
    /// run-status fallback exists to remove, recreated as a testing blind
    /// spot. This test runs the real executor against a real channel,
    /// corrupts the cache it just wrote, and proves `status_of` re-derives
    /// the record from the channel through the sidecar the executor left
    /// behind.
    #[tokio::test]
    async fn corrupt_run_json_falls_back_via_the_executors_channel_sidecar() {
        use mur_channel::ChannelService;

        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let svc = ChannelService::open(mur_home).unwrap();
        let ch = svc.create_for_workflow("sidecar-wf").unwrap();

        let procedure = Procedure {
            variables: vec![],
            steps: vec![step("s1", &[], Some("echo done"))],
        };
        let opts = DagExecOptions {
            run_id: "run-sidecar".into(),
            run_kind: Some(crate::run_status::RunKind::Workflow),
            run_label: "sidecar test run".into(),
            channel_id: Some(ch.id.clone()),
            ..Default::default()
        };
        execute_dag(mur_home, "sidecar-wf", &procedure, &opts)
            .await
            .expect("execute_dag failed");

        // Corrupt the cache the executor just wrote. The sidecar must survive
        // it and carry the rebuild.
        let run_json = crate::run_status::store::run_path(mur_home, "run-sidecar");
        assert!(run_json.exists(), "execute_dag never wrote run.json");
        std::fs::write(&run_json, b"{ this is not json").unwrap();

        let status = crate::run_status::status_of(mur_home, "run-sidecar")
            .unwrap()
            .expect("the executor's sidecar must make a corrupt cache fall back to the channel");
        assert_eq!(
            status.state,
            crate::run_status::State::Done,
            "the channel's Completed transition must be re-derived from its events"
        );
        assert!(
            status.run.last_heartbeat_at.is_none(),
            "a re-derived record must report an unknown heartbeat, never invent one"
        );
    }
}
