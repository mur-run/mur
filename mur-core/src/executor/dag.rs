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

mod graph;
mod run;
mod step;

use crate::channel_writer::ROUTER_AGENT;
pub(crate) use graph::validate_steps;
use graph::*;
pub use run::*;
use step::*;

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
    /// What an unanswered risk-tiered gate does. `None` = auto: defer when
    /// stdin is not a TTY, wait when it is — because an unattended run (daemon
    /// tick, schedule, cron, fleet loop) has nobody to answer inside the wait
    /// window, and waiting there only converts the whole window into a denial
    /// while killing a request a human could still have answered. Set it
    /// explicitly (fleet.yaml `hitl.mode`) when the TTY is a poor proxy for
    /// "somebody is watching".
    pub hitl_unanswered: Option<mur_common::hitl::Unanswered>,
    /// Risk tiers the run's owner pre-approved (fleet.yaml
    /// `hitl.auto_approve_tiers`). Empty = every Ask-tier action still needs a
    /// person. Capped at `write` by `mur_common::hitl::tier_may_be_granted`,
    /// re-checked inside the gate.
    pub hitl_auto_approve_tiers: Vec<mur_common::hitl::RiskTier>,
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
    /// The launching scope's deadline, as an instant (spec §3.4). A delegated
    /// member is handed the REMAINING seconds in `limits.deadline_secs`, so
    /// its clock is the fleet's, not a fresh one. `None` = the member resolves
    /// its own scopes (a workflow step, a run without a deadline).
    pub deadline_at: Option<std::time::Instant>,
    /// Tools the fleet declared its work needs (`fleet.yaml needs:`). Handed
    /// to every delegate as the A2A `needs` parameter so it can fail at
    /// dispatch (spec §3.8) instead of after its budget. Empty = no preflight.
    pub needs: Vec<String>,
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
            hitl_unanswered: None,
            hitl_auto_approve_tiers: Vec::new(),
            env_class_override: None,
            variables: vec![],
            device_id: "cli".to_string(),
            trigger: "manual",
            channel_id: None,
            run_id: String::new(),
            run_kind: None,
            run_label: String::new(),
            max_concurrency: None,
            deadline_at: None,
            needs: Vec::new(),
            on_step: None,
        }
    }
}

/// How a DAG run ended. `Blocked` is deliberately not a failure: nothing went
/// wrong, the run simply reached an action that needs a human and stopped
/// there. It is also not terminal — an approval resumes it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RunOutcome {
    Done,
    Failed,
    Blocked,
}

/// Step-lifecycle event kind for `DagExecOptions.on_step`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepEventKind {
    Started,
    Done,
    Failed,
    /// Waiting on a human: the step did not run, and it did not fail.
    Blocked,
}

/// The default policy when the caller did not state one: nothing can answer an
/// approval prompt in the next few minutes if stdin has no TTY. Daemon ticks,
/// schedules, cron and the fleet loop all land here, which is exactly where
/// waiting out a gate timeout converts the whole window into an automatic
/// denial.
pub(crate) fn default_unanswered() -> mur_common::hitl::Unanswered {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        mur_common::hitl::Unanswered::Wait
    } else {
        mur_common::hitl::Unanswered::Defer
    }
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
    /// Why the step ended this way. Only meaningful on `Failed`.
    pub error: Option<String>,
}

/// Cap on a recorded step failure reason: long enough for a dial error or a
/// stderr tail, short enough that `run.json` stays a record and not a log.
const STEP_ERROR_MAX_CHARS: usize = 500;

/// Cap on the output excerpt mirrored into a channel `ToolResult`.
const CHANNEL_EXCERPT_MAX_CHARS: usize = 2048;

/// `String::truncate` panics off a char boundary and agent output is
/// routinely multibyte, so every cap in this file goes through here.
fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// The reason to record on a terminal step. `None` for a success.
///
/// An empty output still yields a reason: "failed with nothing to say" is
/// itself the finding, and a bare exit code beats an empty field.
fn step_failure_reason(result: &StepResult) -> Option<String> {
    if result.success {
        return None;
    }
    let text = result.output_text.trim();
    Some(if text.is_empty() {
        format!("no output; exit code {}", result.exit_code)
    } else {
        truncate_chars(text, STEP_ERROR_MAX_CHARS)
    })
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
        // Not ended: a blocked step is expected to run once approved, so it
        // keeps no end stamp (`mur job status` shows it as still outstanding).
        StepEventKind::Blocked => (crate::run_status::State::Blocked, None, None),
    };
    if let Some(step) = record.steps.iter_mut().find(|s| s.id == event.id) {
        step.state = state;
        // Assign, never merge: a retry re-arms with `None` and must not leave
        // the previous attempt's reason sitting next to a running state.
        step.error = event.error.clone();
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
        error: event.error.clone(),
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
    deadline_secs: Option<u64>,
    needs: &[String],
) -> serde_json::Value {
    let text = format!("{}{}", text, DELEGATE_REPLY_CONTRACT);
    let mut p = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] },
        "channel_id": channel_id,
        "task_id": child_task_id,
        "idempotency_key": idempotency_key,
    });
    if let Some(n) = deadline_secs {
        p["limits"] = serde_json::json!({ "deadline_secs": n });
    }
    if !needs.is_empty() {
        p["needs"] = serde_json::json!(needs);
    }
    p
}

/// Seconds left on the launching clock, floored at one so a delegate that
/// starts on the deadline is told to stop at once rather than told nothing.
fn remaining_secs(deadline_at: Option<std::time::Instant>, now: std::time::Instant) -> Option<u64> {
    deadline_at.map(|d| d.saturating_duration_since(now).as_secs().max(1))
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

/// Why a runtime guard cut a delegated turn short, if one did.
///
/// The runtime reports a guard stop in `Task.usage.stop_reason`
/// (`task_runner.rs`, the `Completed` branch): `loop_detected`, `stuck`,
/// `deadline`, or `iteration_ceiling`. The task itself still says `Completed`
/// on purpose — partial work is preserved rather than thrown away — so the
/// state field cannot be used to tell a finished turn from an aborted one.
/// This field can.
fn guard_stop(task: &serde_json::Value) -> Option<String> {
    let usage = task.get("usage")?;
    let reason = usage.get("stop_reason")?.as_str()?;
    Some(match usage.get("iterations").and_then(|v| v.as_u64()) {
        Some(n) => format!("{reason} after {n} iterations"),
        None => reason.to_string(),
    })
}

/// The failure the runtime itself reported, if any.
///
/// A2A puts a refusal in `Task.error` (`{code, message}`) and writes NO agent
/// message with it — a runtime that cannot start the turn has nothing to say
/// in the specialist's voice. Reading only `messages` and `usage` therefore
/// produced an empty verdict, and `step_failure_reason`'s empty-output
/// fallback rendered it as `no output; exit code 1`: on 2026-09-20 three
/// deep-research members failed with `cannot_start` and a remedy command in
/// hand, and every surface showed that one useless sentence instead.
///
/// `error: null` is not an error — the field is serialized on success too.
fn task_error(task: &serde_json::Value) -> Option<String> {
    let err = task.get("error")?;
    if err.is_null() {
        return None;
    }
    let message = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let code = err.get("code").and_then(|c| c.as_str()).unwrap_or("");
    Some(match (code.is_empty(), message.is_empty()) {
        // Neither field survived the wire, but a non-null `error` still means
        // the turn failed — say so rather than returning None and reverting
        // to the silence this function exists to end.
        (true, true) => "delegate failed: runtime reported an unlabelled error".to_string(),
        (true, false) => format!("delegate failed: {message}"),
        (false, true) => format!("delegate failed [{code}]"),
        (false, false) => format!("delegate failed [{code}]: {message}"),
    })
}

/// Turn a delegated `Task` into a step verdict.
///
/// Success used to be `!reply.trim().is_empty()` — the ONLY test was whether
/// the specialist said anything at all. A turn that a guard aborted still ends
/// with a summary message, so it scored as a clean `done`: on 2026-09-13 a
/// delegate burned 95K tokens, was stopped by the doom-loop detector after 54
/// iterations, reported "Task 4 is blocked", wrote no code — and the run
/// recorded `"state": "done"`. Three dispatches were spent before anyone
/// looked past the status.
///
/// A guard stop is a failure, not a `blocked`: `blocked` means "waiting on a
/// human, nothing ran" and deliberately skips the ledger and `on_failure`,
/// whereas an aborted turn did run, did spend, and did not deliver.
///
/// Deliberately NOT read here: the `Completion:` checklist that
/// [`DELEGATE_REPLY_CONTRACT`] asks the specialist to end with. Deciding a
/// step's fate by pattern-matching model prose trades one silent wrong answer
/// for another; the structured signal above covers every guard the runtime
/// can apply to itself. An agent that declares itself blocked while ending its
/// turn cleanly still scores as success, and wiring that up needs a structured
/// channel (an A2A task state), not a parser.
fn delegate_result(
    task: &serde_json::Value,
    step_description: &str,
    duration_ms: u64,
) -> StepResult {
    // Reply text is extracted ONLY to fill StepResult.output_text — the
    // specialist already wrote+signed the reply Message itself.
    let reply = extract_agent_reply(task);
    let stopped = guard_stop(task);
    // A reported error is decisive on its own: the refusal path produces no
    // agent message at all, so requiring a non-empty reply would keep losing
    // exactly the failures that explain themselves best.
    let errored = task_error(task);
    let landed = !reply.trim().is_empty() && stopped.is_none() && errored.is_none();
    // Both prefixes can apply; the specialist's own words always come last so
    // partial work is never truncated away by a banner.
    let mut prefixes: Vec<String> = Vec::new();
    if let Some(why) = &errored {
        prefixes.push(why.clone());
    }
    if let Some(why) = &stopped {
        prefixes.push(format!("[delegate stopped short: {why}]"));
    }
    let output_text = if prefixes.is_empty() {
        reply
    } else if reply.trim().is_empty() {
        prefixes.join("\n")
    } else {
        format!("{}\n{reply}", prefixes.join("\n"))
    };
    StepResult {
        exit_code: if landed { 0 } else { 1 },
        output_text,
        duration_ms,
        failed_step: (!landed).then(|| step_description.to_string()),
        success: landed,
        blocked: false,
        // Real tokens the specialist's turn consumed, from Task.usage.
        tokens_used: extract_usage_tokens(task),
    }
}
#[cfg(test)]
mod tests;
