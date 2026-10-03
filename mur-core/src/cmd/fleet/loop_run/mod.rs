//! Phase 2a: the fleet loop. Wraps Phase 1's single iteration in a guarded loop
//! (iteration cap, deadline, stuck-detection, marker/router convergence). The guards
//! live HERE — outside any agent — so the daemon `fleet_tick` (Phase 2b) can
//! reuse the same logic. The live orchestration needs running member agents;
//! the pure guard helpers below are unit-tested.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use mur_common::channel::{ChannelActor, ChannelEvent};
use mur_common::fleet::{Fleet, Job, JobStatus};
use sha2::{Digest, Sha256};

use super::done_policy::{DonePolicy, done_policy};
use super::progress::{
    RunProgress, StepProgress, StepState, classify_phase, iteration_summary_line,
};
use super::run::build_fleet_procedure;
use super::store;
use crate::executor::dag::{StepEvent, StepEventKind};
use crate::executor::delegation::cwd::{RunCwd, routing_note};

mod guarded;
mod synth;

pub use guarded::*;
use synth::*;

/// Diagnostic ceiling on loop iterations (spec §6). Not a setting — the bounds
/// a user sets are `deadline`, `stuck` and `cost_usd` (`mur fleet limits`).
pub const LOOP_ITERATION_CEILING: u32 = 10_000;
/// Tokens one member burns in one ITERATION, used as the iteration-1 forward
/// estimate and as the fail-safe fallback when an iteration reports no usage.
/// Real per-token cost flows back via `PipelineOutput.tokens_used` (summed from
/// each delegate's `Task.usage`), so cumulative spend is actual, not projected;
/// this only seeds the forward budget check before real data exists.
///
/// The old name and value (`EST_TOKENS_PER_TURN`, 8000) modelled a single TURN,
/// but a member does not take one turn per iteration — it runs an agentic loop
/// whose tool results are whole fetched web pages. The gap was about 300x, so
/// the guard waved through an iteration it could not afford and only reported
/// the overrun afterwards: a deep-research run with a $10 ceiling spent $21.52
/// and still exited `Converged`.
///
/// Grounded in three measured deep-research runs, three workers each, on a
/// $0.003/1k model:
///
/// | run | cost | tokens/member | notes |
/// |---|---|---|---|
/// | initial survey, search failing | $21.52 | 2.39M | fell back to whole-page fetches |
/// | continuation of a prior channel | $40.09 | 4.45M | verify + synthesise, not a survey |
/// | initial survey, search working | $32.12 | 3.57M | the clean measurement |
///
/// 3.5M is the middle of those, and projects $31.50 for three members against
/// a measured $32.12 — close enough to gate honestly without refusing budgets
/// a normal run fits inside.
///
/// Note what the clean pair shows: fixing search made a run MORE expensive
/// ($21.52 → $32.12), not less. Search does not replace fetching; it finds
/// more sources worth fetching. An estimate calibrated while search was broken
/// would have under-read every healthy run.
const EST_TOKENS_PER_MEMBER_ITERATION: u64 = 3_500_000;
/// Fallback per-1k-token USD rate when models.yaml has no priced entry and no
/// `MUR_FLEET_COST_PER_1K` override. Deliberately dear (frontier-ish output rate)
/// so the projection errs high → stops early.
const DEFAULT_PRICE_PER_1K: f64 = 0.05;

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopStop {
    /// Goal complete: a structured `done_when: marker:<TEXT>` was emitted by a
    /// member, or (free-text/empty criterion) the router judged it done.
    Converged,
    /// Hit the `LOOP_ITERATION_CEILING` diagnostic ceiling — a runaway.
    MaxIterations,
    /// Hit the wall-clock deadline.
    Deadline,
    /// No agent-authored channel event for the stuck window (spec §3.5).
    Stuck,
    /// Projected cumulative cost would exceed the fleet's budget.
    Budget,
    /// Kill-switch engaged via `mur fleet stop`.
    Stopped,
    /// A commander governance kill (or zero budget-ceiling) halted the loop.
    CommanderKilled,
    /// `done_when: queue-empty` and an iteration found no queued job — the
    /// fleet's work is done because there is none left.
    QueueDrained,
    /// An iteration stopped at an action awaiting human approval. Looping past
    /// it would re-spend router and member LLM calls every iteration to arrive
    /// at the same unanswered question, so the loop stops and hands the
    /// decision back to a person.
    AwaitingApproval,
    /// The iteration's DAG came back `Failed` — every delegated step failed,
    /// or enough of them did that the executor called the run a failure.
    ///
    /// Without this variant the loop read only `Skipped` (blocked) and let a
    /// failed iteration fall through to the synthesis turn. The router would
    /// then emit the convergence marker over an empty evidence set, the loop
    /// broke `Converged`, and `Converged` maps to `State::Done`: on
    /// 2026-09-20 three deep-research workers all failed to start and the run
    /// still recorded `done`, with a synthesized report whose own text
    /// admitted it had received no worker evidence.
    IterationFailed,
}

/// One grammar for `--deadline`, `loop.deadline` and `limits.deadline`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    mur_common::limits::parse_duration(s)
}

/// What bounds this fleet run, resolved across `--flags` → `fleet.yaml
/// limits:` (or its legacy `loop.*`) → `config.yaml limits:` → built-in.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetBounds {
    pub deadline: Duration,
    pub deadline_source: mur_common::limits::Source,
    pub stuck: mur_common::limits::Stuck,
    /// The configured cost cap, before `budget_for` decides whether the
    /// fleet can spend at all.
    pub cost_usd: Option<f64>,
    /// The full resolution this was derived from, kept so a consumer that
    /// needs the budget as a whole (pre-dispatch triage) sees exactly what
    /// the guards will enforce rather than a second, drifting copy of it.
    pub resolved: mur_common::limits::ResolvedLimits,
}

pub fn fleet_bounds(
    mur_home: &Path,
    fleet: &Fleet,
    flag_deadline: Option<&str>,
    flag_budget: Option<f64>,
) -> Result<FleetBounds> {
    let global = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).limits;
    let fleet_limits = fleet.limits_or_legacy();
    let flags = mur_common::limits::Limits {
        deadline: flag_deadline.map(|d| d.trim().to_string()),
        stuck: None,
        cost_usd: flag_budget,
    };
    let r = mur_common::limits::resolve(
        mur_common::limits::Scope::FleetRun,
        &global,
        fleet_limits.as_ref(),
        None,
        &flags,
    )
    .map_err(|e| anyhow::anyhow!("fleet '{}': {e}", fleet.name))?;
    Ok(FleetBounds {
        // FleetRun always resolves a deadline (built-in when nothing is set);
        // the Option exists for the SingleTask/attended case.
        deadline: r
            .deadline
            .value
            .unwrap_or(mur_common::limits::DEFAULT_DEADLINE_FLEET),
        deadline_source: r.deadline.source,
        stuck: r.stuck.value,
        cost_usd: r.cost_usd.value,
        resolved: r,
    })
}

/// Pure pre-iteration guard check. `stuck_for` = time since the last
/// agent-authored channel event. Deadline first: when both are due, the
/// clock the user set explicitly is the one to name.
pub fn check_guards(
    iteration: u32,
    elapsed: Duration,
    deadline: Duration,
    stuck_for: Duration,
    stuck: mur_common::limits::Stuck,
) -> Option<LoopStop> {
    if elapsed >= deadline {
        return Some(LoopStop::Deadline);
    }
    if let mur_common::limits::Stuck::After(limit) = stuck
        && stuck_for >= limit
        && iteration > 0
    {
        return Some(LoopStop::Stuck);
    }
    if iteration >= LOOP_ITERATION_CEILING {
        return Some(LoopStop::MaxIterations);
    }
    None
}

/// Does the router's reply signal completion? True iff a standalone `done`
/// token appears AND no `continue`/negation token does. This fails SAFE: an
/// ambiguous, negated ("not done"), or empty reply returns false ("keep
/// going"), and the cap/deadline/stuck guards still bound the loop. Stopping
/// early on a false positive is worse than one extra iteration.
pub fn is_converged(reply: &str) -> bool {
    let tokens: Vec<String> = reply
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let has = |t: &str| tokens.iter().any(|w| w == t);
    has("done") && !has("continue") && !has("not") && !has("incomplete")
}

/// Has a member emitted `marker` as a SENTINEL — the sole trimmed content of
/// some line — in a channel event newer than `after_seq`?
///
/// Sentinel (own-line) matching, not substring, is deliberate and fail-safe:
/// the marker text is fanned out to every member in the goal, so a member that
/// merely quotes or negates it in prose ("will emit DONE_TOKEN when done",
/// "DONE_TOKEN not yet") must NOT converge the loop. Requiring the marker to be
/// a line by itself makes it an unambiguous deliberate signal and inherently
/// excludes negated/embedded mentions — mirroring `is_converged`'s posture that
/// stopping early on a false positive is worse than one extra iteration. Only
/// `Agent`-authored events count (matching the stuck-detection filter), so the
/// criterion text in the System goal event can't self-trigger either.
pub fn channel_has_marker(events: &[ChannelEvent], marker: &str, after_seq: u64) -> bool {
    events.iter().any(|e| {
        e.seq > after_seq
            && matches!(e.actor, ChannelActor::Agent { .. })
            && e.payload
                .get("text")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.lines().any(|line| line.trim() == marker))
    })
}

/// Projected USD for one iteration: every member burns about
/// `EST_TOKENS_PER_MEMBER_ITERATION` at `price_per_1k`. Used as the iteration-1
/// forward estimate (before any real data) and as the fail-safe fallback when an
/// iteration reports no usage.
pub fn estimate_iteration_cost_usd(members: usize, price_per_1k: f64) -> f64 {
    members as f64 * (EST_TOKENS_PER_MEMBER_ITERATION as f64 / 1000.0) * price_per_1k
}

/// Real USD for an iteration from its actual token total (`PipelineOutput.tokens_used`,
/// input + output summed across delegate turns) at `price_per_1k`.
pub fn iteration_cost_usd(tokens_used: u64, price_per_1k: f64) -> f64 {
    (tokens_used as f64 / 1000.0) * price_per_1k
}

/// Would another iteration (projected `next_cost`) exceed `budget`? Enforced only
/// when budget is `Some(>0)`; stops BEFORE the unaffordable iteration (fail-safe).
pub fn budget_exceeded(spent: f64, next_cost: f64, budget: Option<f64>) -> bool {
    matches!(budget, Some(b) if b > 0.0 && spent + next_cost > b)
}

/// Conservative per-1k-token USD rate for projection: `MUR_FLEET_COST_PER_1K`
/// env → else the dearest output rate in `models.yaml` → else `DEFAULT_PRICE_PER_1K`.
/// The dearest output rate in the registry, in USD per 1k tokens.
///
/// A ceiling rather than a per-model lookup: the guard bills a flat rate
/// against an iteration's whole token count, so it has to be at least as
/// expensive as anything the fleet could have used. Split out from
/// `fleet_price_per_1k` so the ceiling property is testable without touching
/// the filesystem or the process environment.
fn dearest_output_rate(reg: &mur_common::model::ModelRegistry) -> Option<f64> {
    let max = reg
        .models
        .values()
        .filter_map(|m| m.effective_costs().1)
        .fold(0.0_f64, f64::max);
    (max > 0.0).then_some(max)
}

/// Where the guard's flat rate came from. A budget enforced on a guess is
/// still worth enforcing, but the user has to be told it is a guess: a run
/// that stops early and a run that stops on target look identical otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardRate {
    /// `MUR_FLEET_COST_PER_1K`.
    Env,
    /// Dearest output rate in `models.yaml`.
    Registry,
    /// `DEFAULT_PRICE_PER_1K` — nothing in the registry carries a rate.
    Default,
}

pub(super) fn fleet_price_per_1k(mur_home: &Path) -> (f64, GuardRate) {
    if let Ok(v) = std::env::var("MUR_FLEET_COST_PER_1K")
        && let Ok(p) = v.parse::<f64>()
        && p > 0.0
    {
        return (p, GuardRate::Env);
    }
    if let Ok(reg) = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml"))
        && let Some(rate) = dearest_output_rate(&reg)
    {
        return (rate, GuardRate::Registry);
    }
    (DEFAULT_PRICE_PER_1K, GuardRate::Default)
}

/// Resolve this iteration's goal: oldest queued job (marking it Running) beats
/// the standing fleet goal. Returns `(goal_text, Some(job))` when a queued job
/// is claimed, or `(standing_goal, None)` when the queue is empty.
fn iteration_goal(
    mur_home: &Path,
    fleet_name: &str,
    standing_goal: &str,
) -> Result<(String, Option<Job>)> {
    if let Some(mut job) = super::jobs::next_queued(mur_home, fleet_name)? {
        job.status = JobStatus::Running;
        job.started_at = Some(chrono::Utc::now().to_rfc3339());
        super::jobs::save_job(mur_home, fleet_name, &job)?;
        Ok((job.text.clone(), Some(job)))
    } else {
        Ok((standing_goal.to_string(), None))
    }
}

/// The budget the guard enforces: the configured one for a fleet that can
/// spend, none for a fleet that cannot (spec D4). Enforcing a dollar ceiling
/// on local models stopped real runs on a projection of money nobody was
/// paying — the failure mode this exists to remove.
fn budget_for(fleet_budget: Option<f64>, billing: &super::billing::FleetBilling) -> Option<f64> {
    if billing.billable { fleet_budget } else { None }
}

/// The one line a billable fleet running on a deadline alone must see (spec
/// §5, decided 2026-09-12): the choice to run without a dollar ceiling is
/// allowed, never slid into. No total is projected — iteration duration is
/// unknown and a made-up number is worse than the honest rate.
fn no_cap_notice(
    billing: &super::billing::FleetBilling,
    budget: Option<f64>,
    deadline: Option<Duration>,
    rate_per_1k: f64,
) -> Option<String> {
    if !billing.billable || budget.is_some_and(|b| b > 0.0) {
        return None;
    }
    let bound = match deadline {
        Some(d) => format!("the deadline ({}s)", d.as_secs()),
        None => "nothing but the iteration cap".to_string(),
    };
    Some(format!(
        "⚠ billable, no cost cap — bound is {bound}; spend is reported per iteration at ${rate_per_1k}/1k — set --budget-usd to cap it"
    ))
}

/// SHA-256 (hex) of the deciding directive's canonical sign-input, so the audit
/// row binds to exactly the signed directive that was honored. Empty if the
/// nonce has no matching event (defensive).
fn directive_content_sha256(events: &[ChannelEvent], nonce: &str, channel_id: &str) -> String {
    events
        .iter()
        .find(|e| e.idempotency_key.as_deref() == Some(nonce))
        .map(|e| {
            let input = mur_channel::sign::sign_input(
                channel_id,
                &e.actor,
                e.kind,
                &e.payload,
                e.idempotency_key.as_deref(),
            );
            hex::encode(Sha256::digest(&input))
        })
        .unwrap_or_default()
}

/// Record (best-effort) that a commander directive was honored. Never blocks the
/// halt. `content_sha256` binds the row to the exact signed directive.
fn emit_governance_audit(
    mur_home: &Path,
    fleet: &str,
    directive: &str,
    decision: &str,
    nonce: &str,
    content_sha256: &str,
) {
    let root_str = mur_home.to_str();
    if let Ok(audit) = crate::conversations::audit::Audit::open(root_str) {
        let _ = audit.append(
            crate::conversations::audit::AuditAction::Governance {
                fleet: fleet.to_string(),
                directive: directive.to_string(),
                decision: decision.to_string(),
                nonce: nonce.to_string(),
            },
            content_sha256.to_string(),
        );
    }
}

/// Poison-safe lock for the run-progress mutex. The progress data is
/// display-only, so a panicked holder never invalidates it — recover the
/// guard rather than propagating the panic into the loop.
fn lock_progress(p: &Mutex<RunProgress>) -> std::sync::MutexGuard<'_, RunProgress> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// Map the loop's stop reason to the progress file's `outcome` string.
fn outcome_label(stop: LoopStop) -> &'static str {
    match stop {
        LoopStop::Converged => "converged",
        LoopStop::MaxIterations => "max-iterations",
        LoopStop::Deadline => "deadline",
        LoopStop::Stuck => "stuck",
        LoopStop::Budget => "budget",
        LoopStop::Stopped => "stopped",
        LoopStop::CommanderKilled => "commander-killed",
        LoopStop::QueueDrained => "queue-drained",
        LoopStop::AwaitingApproval => "awaiting-approval",
        LoopStop::IterationFailed => "iteration-failed",
    }
}

/// Map a loop stop to the guard vocabulary `triage_calibration` scores
/// against, or `None` when the loop ended for a reason that is not an overrun.
///
/// The two vocabularies are separate on purpose — `outcome_label` is for
/// humans reading progress.json, `GUARD_STOPS` is what the calibration counts
/// — so the translation is explicit here rather than implied by a string that
/// happens to match. `MaxIterations` is the case that would be silently lost:
/// the loop calls it `max-iterations`, the runtime calls the same event
/// `iteration_ceiling`, and an unmapped name simply reads as "no overrun".
///
/// `Budget` returns `None` deliberately: it is not in `GUARD_STOPS`, and
/// `overran()` already catches a blown cap from spend-vs-cap. Naming it here
/// too would not change the verdict, and inventing a guard name the runtime
/// never emits would put a value in the ledger that nothing else can produce.
pub fn calibration_stop_reason(stop: LoopStop) -> Option<&'static str> {
    match stop {
        LoopStop::Deadline => Some("deadline"),
        LoopStop::Stuck => Some("stuck"),
        LoopStop::MaxIterations => Some("iteration_ceiling"),
        // Not overruns: the goal was met, a human intervened, or the queue
        // emptied. Scoring these as triage misses would punish it for runs
        // that went exactly right.
        LoopStop::Converged
        | LoopStop::QueueDrained
        | LoopStop::Stopped
        | LoopStop::CommanderKilled
        | LoopStop::AwaitingApproval
        // Not an overrun either: the work itself failed while every guard
        // stayed within its bounds. Scoring it as a triage miss would blame
        // the sizing call for a member that could not start.
        | LoopStop::IterationFailed
        // See the doc comment: spend-vs-cap already detects this.
        | LoopStop::Budget => None,
    }
}

/// The one-line way out for each stop, in the words of the commands that exist
/// today (`mur fleet settings`, `mur fleet start`, `mur channel approve`). The
/// spec's step 3 rewrites these when `limits:` lands; until then a user who
/// hits a cap must at least be told which knob it was. `None` for the two
/// stops that mean the work is done.
pub fn stop_remedy(stop: LoopStop, fleet: &str) -> Option<String> {
    Some(match stop {
        LoopStop::Converged | LoopStop::QueueDrained => return None,
        LoopStop::IterationFailed => format!(
            "every delegated step failed — read the per-step reason: mur fleet status {fleet} (each step now carries the runtime's own error, not `no output`)"
        ),
        LoopStop::MaxIterations => format!(
            "the {LOOP_ITERATION_CEILING}-iteration safety ceiling — a runaway, not a setting; report it with the run id (mur fleet status {fleet})"
        ),
        LoopStop::Deadline => format!(
            "raise it: mur fleet limits {fleet} --deadline <2h>  (fleet.yaml limits.deadline)"
        ),
        LoopStop::Stuck => format!(
            "no member activity for the stuck window — see what they are waiting on: mur fleet status {fleet}; widen it: mur fleet limits {fleet} --stuck <20m|off>"
        ),
        LoopStop::Budget => format!(
            "raise it: mur fleet limits {fleet} --cost-usd <USD>  (fleet.yaml limits.cost_usd)"
        ),
        LoopStop::Stopped => format!("cleared by: mur fleet start {fleet}"),
        LoopStop::CommanderKilled => {
            format!("a commander directive halted {fleet} — inspect it: mur commander status")
        }
        LoopStop::AwaitingApproval => {
            format!("a member is waiting on you: mur channel approve fleet-{fleet} <hitl_id>")
        }
    })
}

/// The channel terminal state a stop implies, in the kebab-case wire form
/// `channel_terminal_status` and the rail already fold. Done is `completed`; a
/// kill is `canceled`; waiting on a person is `input-required`; a guard trip
/// is `failed`, because the goal was not reached — the run ending is not the
/// same as the work being done.
/// The run state a stop implies — the same map `terminal_state_for` gives
/// the channel, in the run ledger's vocabulary.
pub fn loop_terminal_state(stop: LoopStop) -> crate::run_status::State {
    use crate::run_status::State;
    match stop {
        LoopStop::Converged | LoopStop::QueueDrained => State::Done,
        LoopStop::AwaitingApproval => State::Blocked,
        LoopStop::Stopped | LoopStop::CommanderKilled => State::Stopped,
        LoopStop::MaxIterations
        | LoopStop::Deadline
        | LoopStop::Stuck
        | LoopStop::Budget
        | LoopStop::IterationFailed => State::Failed,
    }
}

fn terminal_state_for(stop: LoopStop) -> &'static str {
    match stop {
        LoopStop::Converged | LoopStop::QueueDrained => "completed",
        LoopStop::Stopped | LoopStop::CommanderKilled => "canceled",
        LoopStop::AwaitingApproval => "input-required",
        LoopStop::MaxIterations
        | LoopStop::Deadline
        | LoopStop::Stuck
        | LoopStop::Budget
        | LoopStop::IterationFailed => "failed",
    }
}

/// `mur fleet run --loop`: run guarded iterations until the router converges or
/// a guard trips. Requires the member + router agents to be running.
/// `cwd`: see [`run_guarded`].
#[allow(clippy::too_many_arguments)]
pub async fn cmd_fleet_run_loop(
    mur_home: &Path,
    name: &str,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    run_id: Option<String>,
    goal_override: Option<String>,
    cwd: Option<RunCwd>,
) -> Result<()> {
    let (stop, iteration, spent) = run_guarded(
        mur_home,
        name,
        max_iterations,
        deadline,
        budget_usd,
        run_id,
        goal_override,
        cwd,
    )
    .await?;
    // Read back what the run recorded rather than recomputing billing: the
    // figure and its "was this actually charged" label must come from the
    // same place, or this line can contradict the panel again.
    let billable = super::progress::load_view(mur_home, name).and_then(|v| v.progress.billable);
    println!(
        "fleet '{}' loop stopped after {iteration} iteration(s), cost {}: {stop:?}",
        name,
        super::progress::fmt_spend(spent, billable)
    );
    Ok(())
}

#[cfg(test)]
mod tests;
