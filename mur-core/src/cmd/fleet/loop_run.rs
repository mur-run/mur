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

/// Iterations with no new agent activity before the loop gives up.
const STUCK_LIMIT: u32 = 2;
/// Default iteration cap when neither the CLI flag nor fleet.yaml sets one.
const DEFAULT_MAX_ITERATIONS: u32 = 8;
/// Conservative per-turn token estimate, used as the iteration-1 forward estimate
/// and the fail-safe fallback when an iteration reports no usage. Real per-token
/// cost now flows back via `PipelineOutput.tokens_used` (summed from each
/// delegate's `Task.usage`), so cumulative spend is actual, not projected; this
/// estimate only seeds the forward budget check before real data exists.
const EST_TOKENS_PER_TURN: u64 = 8000;
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
    /// Hit the iteration cap.
    MaxIterations,
    /// Hit the wall-clock deadline.
    Deadline,
    /// `STUCK_LIMIT` consecutive iterations produced no new agent activity.
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
}

/// Parse a humantime-ish duration: `30s`, `5m`, `2h`, `1d`, or a bare integer
/// (= seconds). Returns None on anything else. (No `humantime` dependency.)
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = num.parse().ok()?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        "d" => n.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(secs))
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

/// Pure pre-iteration guard check. `iteration` = number of iterations already
/// completed. Returns Some(stop) to halt before running another.
pub fn check_guards(
    iteration: u32,
    max_iterations: u32,
    elapsed: Duration,
    deadline: Option<Duration>,
    stuck_count: u32,
) -> Option<LoopStop> {
    if iteration >= max_iterations {
        return Some(LoopStop::MaxIterations);
    }
    if let Some(d) = deadline
        && elapsed >= d
    {
        return Some(LoopStop::Deadline);
    }
    if stuck_count >= STUCK_LIMIT {
        return Some(LoopStop::Stuck);
    }
    None
}

/// Resolve the effective iteration cap: CLI flag > fleet.yaml loop.max_iterations > default.
fn effective_max_iterations(flag: Option<u32>, fleet: &Fleet) -> u32 {
    flag.or_else(|| fleet.loop_cfg.as_ref().map(|l| l.max_iterations))
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_ITERATIONS)
}

/// Resolve the effective deadline: CLI flag > fleet.yaml loop.deadline > none.
fn effective_deadline(flag: Option<&str>, fleet: &Fleet) -> Option<Duration> {
    flag.and_then(parse_duration).or_else(|| {
        fleet
            .loop_cfg
            .as_ref()
            .map(|l| l.deadline.as_str())
            .filter(|s| !s.is_empty())
            .and_then(parse_duration)
    })
}

/// Projected USD for one iteration: every member takes a ~`EST_TOKENS_PER_TURN`
/// turn at `price_per_1k`. Used as the iteration-1 forward estimate (before any
/// real data) and as the fail-safe fallback when an iteration reports no usage.
pub fn estimate_iteration_cost_usd(members: usize, price_per_1k: f64) -> f64 {
    members as f64 * (EST_TOKENS_PER_TURN as f64 / 1000.0) * price_per_1k
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

fn fleet_price_per_1k(mur_home: &Path) -> (f64, GuardRate) {
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

/// Resolve the effective budget USD: CLI flag > fleet.yaml `loop.budget_usd` > none.
fn effective_budget(flag: Option<f64>, fleet: &Fleet) -> Option<f64> {
    flag.or_else(|| fleet.loop_cfg.as_ref().map(|l| l.budget_usd))
        .filter(|&b| b > 0.0)
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
    }
}

/// Inner guarded loop: runs iterations until a stop reason fires. Returns
/// `(stop, iterations_completed, spent_usd)`. Extracted so tests can call it
/// directly and inspect the `LoopStop` without going through the print layer.
pub async fn run_guarded(
    mur_home: &Path,
    name: &str,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
) -> Result<(LoopStop, u32, f64)> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        anyhow::bail!("fleet '{name}' has no members");
    }

    // Best-effort program-deps preflight — informational only, never blocks
    // the loop. A load/aggregate error is swallowed.
    let _ = (|| -> Result<()> {
        let deps = crate::cmd::deps::aggregate_fleet(mur_home, name)?;
        let report = crate::cmd::deps::doctor::build_report(&deps, mur_home);
        if crate::cmd::deps::doctor::missing_count(&report) > 0 {
            eprintln!(
                "warning: fleet '{name}' has missing program dependencies — run `mur fleet doctor {name}` for details or `mur fleet install-deps {name}` to install them."
            );
        }
        Ok(())
    })();

    let max_iter = effective_max_iterations(max_iterations, &fleet);
    let deadline = effective_deadline(deadline.as_deref(), &fleet);
    let budget = effective_budget(budget_usd, &fleet);
    let price_per_1k = fleet_price_per_1k(mur_home);
    if let (rate, GuardRate::Default) = price_per_1k
        && budget.is_some()
    {
        // Enforcing on a guess is better than not enforcing, but silently
        // enforcing on one turns "stopped on budget" into a number the user
        // has no way to reconcile against a real bill.
        println!(
            "  ⚠ budget enforced at the default ${rate}/1k — no model in models.yaml \
             carries a rate, so spend is a guess. `mur model add` records a real one."
        );
    }
    let price_per_1k = price_per_1k.0;
    // Forward estimate before any real data (and the fail-safe fallback when an
    // iteration reports no token usage), so spend can never silently under-count.
    let projection = estimate_iteration_cost_usd(fleet.members.len(), price_per_1k);
    // Real cumulative spend, accumulated from each iteration's actual token usage.
    let mut spent = 0.0_f64;
    let start = Instant::now();
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let mut last_seq = svc
        .load_events(&fleet.channel_id)?
        .last()
        .map(|e| e.seq)
        .unwrap_or(0);
    // Baseline for the structured `done_when: marker:<TEXT>` check: only events
    // produced during THIS run (seq > start_seq) count, so a marker left in the
    // channel by a previous run can't make the loop converge instantly.
    let start_seq = last_seq;
    let mut iteration = 0u32;
    let mut stuck = 0u32;

    // Load commander keys once; empty = governance inert.
    let commander_keys = crate::cmd::commander::accepted_pubkeys(mur_home);
    let governed = !commander_keys.is_empty();

    // ── Run progress (deep-research UX): one best-effort JSON the run output +
    // `mur deep-research` panel render from. Every write is best-effort — a
    // failure must never fail, slow, or change the loop (see RunProgress::save).
    let progress = Arc::new(Mutex::new(RunProgress {
        schema_version: 1,
        run_id: uuid::Uuid::now_v7().to_string(),
        question: fleet.goal.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        outcome: None,
        iteration: 0,
        model: fleet
            .members
            .first()
            .and_then(|m| mur_common::agent::AgentProfile::load(mur_home, m).ok())
            .and_then(|p| p.model_ref),
        budget_usd: budget,
        spend_usd: 0.0,
        steps: vec![],
    }));
    lock_progress(&progress).save(mur_home, name);

    let stop = loop {
        // Commander governance (highest priority). Fail-closed: a channel read
        // error halts rather than running ungoverned.
        let mut commander_ceiling: Option<f64> = None;
        if governed {
            let events = match svc.load_events(&fleet.channel_id) {
                Ok(e) => e,
                Err(_) => {
                    // Fail-closed: cannot read governance ⇒ halt, but label the
                    // audit as a read error, not a confirmed commander kill.
                    // Reachable only via a mid-loop FS-level read fault: a missing
                    // channel reads as Ok(empty) and corrupt lines are silently
                    // skipped (store::load_events filter_map), so only a genuine
                    // read failure errors here. The same fault AT ENTRY surfaces via
                    // the `?` on the last_seq load above → run_guarded returns Err →
                    // the caller never runs the loop (also fail-closed). Not
                    // unit-tested: portable FS-fault injection is brittle (mirrors
                    // the daemon's analogous Err arm in fleet_tick::due_fleets).
                    emit_governance_audit(mur_home, name, "read_error", "fail_closed", "", "");
                    break LoopStop::CommanderKilled;
                }
            };
            let gov = mur_channel::governance::fold_governance(
                &events,
                &fleet.channel_id,
                name,
                &commander_keys,
            );
            if gov.killed {
                let nonce = gov.kill_nonce.as_deref().unwrap_or("");
                let csum = directive_content_sha256(&events, nonce, &fleet.channel_id);
                emit_governance_audit(mur_home, name, "kill", "halted", nonce, &csum);
                break LoopStop::CommanderKilled;
            }
            // A zero budget ceiling is a budget halt (spec §6), not a kill.
            if matches!(gov.budget_ceiling, Some(c) if c == 0.0) {
                let nonce = gov.budget_nonce.as_deref().unwrap_or("");
                let csum = directive_content_sha256(&events, nonce, &fleet.channel_id);
                emit_governance_audit(mur_home, name, "budget_ceiling", "capped", nonce, &csum);
                break LoopStop::Budget;
            }
            commander_ceiling = gov.budget_ceiling;
        }

        // Kill-switch: a `mur fleet stop` between iterations halts here.
        if super::control::is_stopped(mur_home, name) {
            break LoopStop::Stopped;
        }
        if let Some(stop) = check_guards(iteration, max_iter, start.elapsed(), deadline, stuck) {
            break stop;
        }
        // Budget guard: stop before an iteration we can't afford. `spent` is the
        // REAL cost so far; the forward estimate is the observed average once we
        // have data (so the loop uses the true budget instead of halting on an
        // inflated projection), falling back to the projection for iteration 1.
        let next_cost = if iteration > 0 {
            spent / iteration as f64
        } else {
            projection
        };
        let effective_budget = match (budget, commander_ceiling) {
            (Some(l), Some(c)) => Some(l.min(c)),
            (None, Some(c)) => Some(c),
            (l, None) => l,
        };
        if budget_exceeded(spent, next_cost, effective_budget) {
            break LoopStop::Budget;
        }
        println!("── fleet '{}' iteration {} ──", name, iteration + 1);

        // Router plans this iteration (seeing prior state); falls back to broadcast.
        let pre_events = svc.load_events(&fleet.channel_id).unwrap_or_default();
        // Drain job queue: oldest queued job is this iteration's goal; else standing goal.
        let (iter_goal, mut active_job) = iteration_goal(mur_home, name, &fleet.goal)?;

        // `done_when: queue-empty` — a drained queue IS the completion
        // condition. Checked here, ahead of `plan_via_router` and every other
        // model call, so a cron tick that wakes to an empty queue costs nothing
        // rather than costing a full iteration. Stuck-detection cannot stand in
        // for this: a member replying "what should I run?" counts as progress,
        // so `stuck` resets and the loop runs to the iteration cap.
        if active_job.is_none()
            && let Some(lc) = fleet.loop_cfg.as_ref()
            && done_policy(&lc.done_when) == DonePolicy::QueueEmpty
        {
            println!("── fleet '{name}': job queue empty — nothing to do ──");
            break LoopStop::QueueDrained;
        }
        let planning_fleet = mur_common::fleet::Fleet {
            goal: iter_goal.clone(),
            ..fleet.clone()
        };
        let proc = super::plan::plan_via_router(mur_home, &planning_fleet, &iter_goal, &pre_events)
            .unwrap_or_else(|| {
                build_fleet_procedure(&iter_goal, &fleet.members, fleet.parallel.as_ref())
                    .expect("members validated by caller guard")
            });
        // Record this iteration's planned steps as Pending (makes "N pending"
        // real) — replacing the prior iteration's so counts reflect the run now.
        {
            let mut g = lock_progress(&progress);
            g.iteration = iteration + 1;
            g.steps = proc
                .steps
                .iter()
                .enumerate()
                .map(|(i, s)| StepProgress {
                    id: s.id.clone().unwrap_or_else(|| i.to_string()),
                    worker: s.delegate_to.clone(),
                    phase: classify_phase(&s.description),
                    desc: s.description.chars().take(120).collect(),
                    state: StepState::Pending,
                    cost_usd: None,
                    started_at: None,
                    ended_at: None,
                })
                .collect();
            g.save(mur_home, name);
        }
        // Display-only step observer: mutate the shared progress + print one log
        // line per completed step. Best-effort throughout — the closure never
        // panics (poison-safe lock) and never affects execution.
        let step_progress = progress.clone();
        let step_home = mur_home.to_path_buf();
        let step_fleet = name.to_string();
        let on_step: Arc<dyn Fn(StepEvent) + Send + Sync> = Arc::new(move |e: StepEvent| {
            let mut g = step_progress.lock().unwrap_or_else(|x| x.into_inner());
            let Some(sp) = g.steps.iter_mut().find(|s| s.id == e.id) else {
                return;
            };
            let now = chrono::Utc::now().to_rfc3339();
            match e.kind {
                StepEventKind::Started => {
                    sp.state = StepState::Running;
                    sp.started_at = Some(now);
                }
                StepEventKind::Done | StepEventKind::Failed => {
                    let done = e.kind == StepEventKind::Done;
                    sp.state = if done {
                        StepState::Done
                    } else {
                        StepState::Failed
                    };
                    if e.tokens_used > 0 {
                        sp.cost_usd = Some(iteration_cost_usd(e.tokens_used, price_per_1k));
                    }
                    // `✓ s2 research dr_worker_2 $0.08 42s`
                    let mark = if done { '✓' } else { '✗' };
                    let phase = sp.phase.label();
                    let worker = sp.worker.clone().unwrap_or_default();
                    let cost = sp.cost_usd.map(|c| format!(" ${c:.2}")).unwrap_or_default();
                    let elapsed = sp
                        .started_at
                        .as_deref()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .and_then(|st| {
                            chrono::DateTime::parse_from_rfc3339(&now)
                                .ok()
                                .map(|en| (en - st).num_seconds())
                        })
                        .filter(|s| *s >= 0)
                        .map(|s| format!(" {s}s"))
                        .unwrap_or_default();
                    sp.ended_at = Some(now);
                    println!("  {mark} {} {phase} {worker}{cost}{elapsed}", e.id);
                }
            }
            g.save(&step_home, &step_fleet);
        });
        let opts = crate::executor::dag::DagExecOptions {
            // Fail-closed on the unattended loop path: never blanket-approve.
            // (No risk tier on fan-out steps today; this guards future
            // router-emitted risk steps. Best-practice audit / OWASP ASI06.)
            yes: false,
            channel_id: Some(fleet.channel_id.clone()),
            // uuid nonce so concurrent `--loop` runs don't collide on the
            // channel's idempotency-key dedup (the iteration stays for readability).
            run_id: format!("loop-{}-{}-{}", name, uuid::Uuid::now_v7(), iteration),
            run_kind: Some(crate::run_status::RunKind::Fleet),
            run_label: format!("fleet {name} iter {iteration}"),
            on_step: Some(on_step),
            ..Default::default()
        };
        let out =
            crate::executor::dag::execute_dag(mur_home, &format!("fleet:{name}"), &proc, &opts)
                .await?;
        iteration += 1;
        // Terminal stamp: mark the queued job Done with the result of this iteration.
        if let Some(job) = active_job.as_mut() {
            job.run_id = Some(opts.run_id.clone());
            job.finished_at = Some(chrono::Utc::now().to_rfc3339());
            job.status = JobStatus::Done;
            job.result = out.output_text.clone().filter(|t| !t.is_empty());
            let _ = super::jobs::save_job(mur_home, name, job);
        }
        // Account REAL cost from this iteration's token usage. A 0-token result
        // (older runtime, stub, or a reply that carried no usage) falls back to
        // the projection so the budget guard never silently under-counts.
        spent += if out.tokens_used > 0 {
            iteration_cost_usd(out.tokens_used, price_per_1k)
        } else {
            projection
        };
        // Roll cumulative spend into the progress file and print the summary.
        {
            let mut g = lock_progress(&progress);
            g.spend_usd = spent;
            g.save(mur_home, name);
            println!("{}", iteration_summary_line(&g));
        }

        // Stuck-detection: did this iteration add any new agent-authored event?
        let events = svc.load_events(&fleet.channel_id)?;
        let progressed = events
            .iter()
            .any(|e| e.seq > last_seq && matches!(e.actor, ChannelActor::Agent { .. }));
        last_seq = events.last().map(|e| e.seq).unwrap_or(last_seq);
        if progressed {
            stuck = 0;
        } else {
            stuck += 1;
        }

        // Convergence: three policies, dispatched from the same `done_when`
        // string `done_policy()` classified against above. `Marker` is checked
        // deterministically against this run's channel events (no LLM, no
        // trusting the router's self-assessment). `QueueEmpty` has nothing to
        // check here — the drained-queue break above is its only stop, so
        // falling through to the router would both cost a call this policy
        // promises not to make and risk a wrong DONE (the router sees the
        // channel, not the queue, and a member reporting its own completion
        // reads a lot like the fleet's). `Router` is the fallback: a failed ask
        // (e.g. router down) is treated as "continue", and the cap/deadline/
        // stuck guards still bound the loop either way.
        let done_when = fleet
            .loop_cfg
            .as_ref()
            .map(|l| l.done_when.as_str())
            .unwrap_or("");
        let converged = match done_policy(done_when) {
            DonePolicy::Marker(m) => channel_has_marker(&events, m, start_seq),
            DonePolicy::QueueEmpty => false, // the drained-queue break above is the only stop for this policy
            DonePolicy::Router => ask_router_done(mur_home, &fleet, &events).unwrap_or(false),
        };
        if converged {
            break LoopStop::Converged;
        }
    };

    // Stamp the terminal state onto the progress file — kept as the last-run
    // record (overwritten by the next run). Best-effort.
    {
        let mut g = lock_progress(&progress);
        g.finished_at = Some(chrono::Utc::now().to_rfc3339());
        g.outcome = Some(outcome_label(stop).to_string());
        g.iteration = iteration;
        g.spend_usd = spent;
        g.save(mur_home, name);
    }
    Ok((stop, iteration, spent))
}

/// `mur fleet run --loop`: run guarded iterations until the router converges or
/// a guard trips. Requires the member + router agents to be running.
pub async fn cmd_fleet_run_loop(
    mur_home: &Path,
    name: &str,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
) -> Result<()> {
    let (stop, iteration, spent) =
        run_guarded(mur_home, name, max_iterations, deadline, budget_usd).await?;
    println!(
        "fleet '{}' loop stopped after {iteration} iteration(s) (~${spent:.2} spent): {stop:?}",
        name
    );
    Ok(())
}

/// Ask the router agent whether the goal is complete. Streams a one-word reply.
fn ask_router_done(mur_home: &Path, fleet: &Fleet, events: &[ChannelEvent]) -> Result<bool> {
    let recent: String = events
        .iter()
        .rev()
        .take(8)
        .rev()
        .filter_map(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let done_when = fleet
        .loop_cfg
        .as_ref()
        .map(|l| l.done_when.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(none given — judge from the goal)");
    let prompt = format!(
        "You are the router for fleet '{}'.\nGoal: {}\nDone-criterion: {}\nRecent channel activity:\n{}\n\nIs the goal complete? Reply with exactly one word: DONE or CONTINUE.",
        fleet.name,
        fleet.goal,
        done_when,
        if recent.is_empty() {
            "(none yet)"
        } else {
            &recent
        },
    );
    let params = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": prompt }] }
    });
    let mut out = String::new();
    crate::a2a_dial::dial_message_streaming(
        mur_home,
        fleet.router_or_concierge(),
        params,
        |delta, _thinking, _id| out.push_str(delta),
        |_hitl| {},
        |_step| {},
    )?;
    Ok(is_converged(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_event(seq: u64, id: &str, text: &str) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: chrono::Utc::now(),
            actor: ChannelActor::Agent { id: id.into() },
            kind: mur_common::channel::EventKind::Note,
            payload: serde_json::json!({ "text": text }),
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    fn sys_event(seq: u64, text: &str) -> ChannelEvent {
        ChannelEvent {
            seq,
            ts: chrono::Utc::now(),
            actor: ChannelActor::System,
            kind: mur_common::channel::EventKind::Note,
            payload: serde_json::json!({ "text": text }),
            idempotency_key: None,
            sig: None,
            key_version: None,
        }
    }

    #[test]
    fn channel_has_marker_matches_member_events_after_baseline() {
        let evs = vec![
            // System goal event MENTIONS the token — must NOT self-trigger.
            sys_event(1, "goal: emit DONE_TOKEN when finished"),
            agent_event(2, "qa", "still working"),
            agent_event(3, "pm", "all green\nDONE_TOKEN"), // sentinel on its own line
        ];
        // a member emitted the marker as a sentinel after baseline 0 → converged
        assert!(channel_has_marker(&evs, "DONE_TOKEN", 0));
        // baseline at seq 3 excludes this-run events → not converged (stale-run guard)
        assert!(!channel_has_marker(&evs, "DONE_TOKEN", 3));
        // the System goal event alone never counts (Agent-authored only)
        assert!(!channel_has_marker(&evs[..1], "DONE_TOKEN", 0));
        // absent marker
        assert!(!channel_has_marker(&evs, "NOT_PRESENT", 0));
    }

    #[test]
    fn channel_has_marker_rejects_prose_mentions_of_the_marker() {
        // The marker is fanned out to members in the goal, so prose that quotes
        // or negates it must NOT converge — only a deliberate own-line sentinel.
        let planning = vec![agent_event(
            2,
            "qa",
            "I will emit DONE_TOKEN when tests pass",
        )];
        assert!(!channel_has_marker(&planning, "DONE_TOKEN", 0));
        let negated = vec![agent_event(2, "qa", "DONE_TOKEN not yet emitted")];
        assert!(!channel_has_marker(&negated, "DONE_TOKEN", 0));
        let embedded = vec![agent_event(2, "qa", "see ABANDONED_TOKENS below")];
        assert!(!channel_has_marker(&embedded, "DONE_TOKEN", 0));
        // but a trailing-whitespace sentinel line still converges (trimmed)
        let sentinel = vec![agent_event(2, "qa", "done:\n  DONE_TOKEN  ")];
        assert!(channel_has_marker(&sentinel, "DONE_TOKEN", 0));
    }

    #[test]
    fn parse_duration_units_and_bare_seconds() {
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration(" 2h "), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("2y"), None);
        assert_eq!(parse_duration("h"), None);
    }

    #[test]
    fn is_converged_detects_done_word_only() {
        assert!(is_converged("DONE"));
        assert!(is_converged("done"));
        assert!(is_converged("The goal is DONE."));
        assert!(is_converged("Done — all issues closed"));
        assert!(!is_converged("CONTINUE"));
        assert!(!is_converged("not done yet")); // negation guard → continue
        assert!(!is_converged("done but CONTINUE")); // continue token wins
        assert!(!is_converged("undone")); // substring, not a token → false
        assert!(!is_converged(""));
    }

    #[test]
    fn check_guards_precedence_and_trips() {
        // under all limits → keep going
        assert_eq!(check_guards(0, 8, Duration::from_secs(0), None, 0), None);
        // iteration cap
        assert_eq!(
            check_guards(8, 8, Duration::from_secs(0), None, 0),
            Some(LoopStop::MaxIterations)
        );
        // deadline
        assert_eq!(
            check_guards(
                1,
                8,
                Duration::from_secs(10),
                Some(Duration::from_secs(5)),
                0
            ),
            Some(LoopStop::Deadline)
        );
        // not yet past deadline
        assert_eq!(
            check_guards(
                1,
                8,
                Duration::from_secs(3),
                Some(Duration::from_secs(5)),
                0
            ),
            None
        );
        // stuck
        assert_eq!(
            check_guards(1, 8, Duration::from_secs(0), None, STUCK_LIMIT),
            Some(LoopStop::Stuck)
        );
        // cap takes precedence over stuck
        assert_eq!(
            check_guards(8, 8, Duration::from_secs(0), None, STUCK_LIMIT),
            Some(LoopStop::MaxIterations)
        );
    }

    #[test]
    fn estimate_and_budget_guard() {
        // 2 members × 8000/1000 × 0.05 = 0.8 / iteration
        assert!((estimate_iteration_cost_usd(2, 0.05) - 0.8).abs() < 1e-9);
        // no budget / zero budget → never blocks
        assert!(!budget_exceeded(100.0, 5.0, None));
        assert!(!budget_exceeded(100.0, 5.0, Some(0.0)));
        // stops BEFORE exceeding
        assert!(budget_exceeded(0.9, 0.2, Some(1.0))); // 1.1 > 1.0
        assert!(!budget_exceeded(0.7, 0.2, Some(1.0))); // 0.9 <= 1.0
        // even iteration 0 unaffordable → blocked immediately
        assert!(budget_exceeded(0.0, 2.0, Some(1.0)));
    }

    #[test]
    fn real_iteration_cost_from_tokens() {
        // 10_000 tokens × $0.05/1k = $0.50
        assert!((iteration_cost_usd(10_000, 0.05) - 0.5).abs() < 1e-9);
        // zero tokens → zero (caller falls back to the projection to avoid
        // under-counting; the helper itself is exact).
        assert_eq!(iteration_cost_usd(0, 0.05), 0.0);
        // real cost is typically well under the 8000-tok/member projection:
        // a 1200-token iteration costs far less than the 1-member projection.
        assert!(iteration_cost_usd(1200, 0.05) < estimate_iteration_cost_usd(1, 0.05));
    }

    #[test]
    fn fleet_price_per_1k_env_then_default() {
        let tmp = tempfile::tempdir().unwrap();
        // env override wins (nextest isolates per-process, so this is safe)
        unsafe { std::env::set_var("MUR_FLEET_COST_PER_1K", "0.123") };
        let (rate, src) = fleet_price_per_1k(tmp.path());
        assert!((rate - 0.123).abs() < 1e-9);
        assert_eq!(src, GuardRate::Env);
        // no env + no models.yaml → documented default, and it says so
        unsafe { std::env::remove_var("MUR_FLEET_COST_PER_1K") };
        let (rate, src) = fleet_price_per_1k(tmp.path());
        assert!((rate - DEFAULT_PRICE_PER_1K).abs() < 1e-9);
        assert_eq!(src, GuardRate::Default);
    }

    /// The budget guard bills one flat rate against an iteration's whole token
    /// count, while the cost report prices each component separately. If the
    /// guard's rate ever dips below what the report would charge for a token of
    /// any model in the registry, the fleet overspends its budget by exactly
    /// that ratio — and nothing on screen says so. Both bugs this branch fixed
    /// were that failure: a rate in the wrong field made the guard 5x cheap,
    /// and a stripped registry would have made it 57x cheap.
    #[test]
    fn guard_rate_is_never_below_what_the_report_charges() {
        use crate::cmd::conversations_cost_report::resolve_rates;
        use mur_common::model::{ModelEntry, ModelRegistry};

        let entry = |input: f64, output: f64| ModelEntry {
            provider: "anthropic".into(),
            model: "m".into(),
            input_cost_per_1k: Some(input),
            output_cost_per_1k: Some(output),
            ..Default::default()
        };
        let mut reg = ModelRegistry::default();
        for (alias, wire, i, o) in [
            ("opus", "claude-opus-5", 0.005, 0.025),
            ("sonnet", "claude-sonnet-5", 0.003, 0.015),
            ("haiku", "claude-haiku-4-5", 0.001, 0.005),
            ("ds", "deepseek-v4-pro", 0.000435, 0.00087),
        ] {
            reg.models.insert(
                alias.into(),
                ModelEntry {
                    model: wire.into(),
                    ..entry(i, o)
                },
            );
        }

        let guard = dearest_output_rate(&reg).expect("registry is priced");
        for e in reg.models.values() {
            // Per 1k, against the report's per-1M rates.
            let ((r_in, r_out, r_cw, r_cr), _) =
                resolve_rates(&e.model, Some(&reg)).expect("report prices it");
            for (label, report_rate) in [
                ("input", r_in),
                ("output", r_out),
                ("cache_write", r_cw),
                ("cache_read", r_cr),
            ] {
                assert!(
                    guard >= report_rate / 1000.0,
                    "guard {guard}/1k is under the {label} rate the report charges for {} \
                     ({}/1k) — a fleet on that model overspends its budget",
                    e.model,
                    report_rate / 1000.0
                );
            }
        }
    }

    /// A registry that prices nothing must not collapse the ceiling to zero —
    /// the documented default is deliberately dearer than any real model.
    #[test]
    fn unpriced_registry_yields_no_ceiling_rather_than_a_free_one() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "local".into(),
            ModelEntry {
                provider: "openai".into(),
                model: "Qwen3.5-4B-MLX-4bit".into(),
                ..Default::default()
            },
        );
        assert_eq!(dearest_output_rate(&reg), None);
        // Const item: editing the default below a real model's rate fails the
        // BUILD, not just this test.
        const _: () = assert!(DEFAULT_PRICE_PER_1K > 0.025);
    }

    #[test]
    fn effective_max_iterations_precedence() {
        let mut f = Fleet {
            name: "x".into(),
            display_name: String::new(),
            goal: String::new(),
            router: None,
            members: vec![],
            channel_id: "fleet-x".into(),
            team_id: None,
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            requires_programs: vec![],
        };
        // default when nothing set
        assert_eq!(effective_max_iterations(None, &f), DEFAULT_MAX_ITERATIONS);
        // fleet.yaml value
        f.loop_cfg = Some(mur_common::fleet::FleetLoop {
            trigger: "manual".into(),
            max_iterations: 3,
            budget_usd: 0.0,
            deadline: String::new(),
            done_when: String::new(),
        });
        assert_eq!(effective_max_iterations(None, &f), 3);
        // CLI flag wins
        assert_eq!(effective_max_iterations(Some(5), &f), 5);
        // a 0 in fleet.yaml is ignored → default
        if let Some(l) = f.loop_cfg.as_mut() {
            l.max_iterations = 0;
        }
        assert_eq!(effective_max_iterations(None, &f), DEFAULT_MAX_ITERATIONS);
    }

    #[test]
    fn iteration_goal_drains_queue_then_falls_back_to_standing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("standing".into()),
            None,
        )
        .unwrap();

        // empty queue → standing goal, no job
        let (g, j) = iteration_goal(home, "dev", "standing").unwrap();
        assert_eq!(g, "standing");
        assert!(j.is_none());

        // queued job → job text, marked running
        super::super::jobs::enqueue_job(home, "dev", "job-1", "cli").unwrap();
        let (g, j) = iteration_goal(home, "dev", "standing").unwrap();
        assert_eq!(g, "job-1");
        assert_eq!(j.unwrap().status, mur_common::fleet::JobStatus::Running);
    }

    /// Test seam: run one guarded iteration and return the stop reason.
    async fn run_loop_for_test(home: &Path) -> LoopStop {
        run_guarded(home, "dev", Some(1), None, None)
            .await
            .map(|(stop, _, _)| stop)
            .unwrap_or(LoopStop::MaxIterations)
    }

    #[tokio::test]
    async fn progress_file_written_with_outcome_on_guard_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "research question".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()])
            .unwrap();
        // Kill-switch: the loop stops before any delegation, so no live agent
        // is needed — but the before-loop and exit progress writes still run.
        crate::cmd::fleet::control::cmd_fleet_stop(home, "dev").unwrap();

        let stop = run_loop_for_test(home).await;
        assert_eq!(stop, LoopStop::Stopped);

        let (p, _) = crate::cmd::fleet::progress::load(home, "dev").expect("progress file written");
        assert_eq!(p.schema_version, 1);
        assert_eq!(p.question, "research question");
        assert!(p.finished_at.is_some());
        assert_eq!(p.outcome.as_deref(), Some("stopped"));
    }

    #[tokio::test]
    async fn commander_kill_halts_loop_and_local_start_cannot_clear_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // commander identity + pinned key
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&cdir)
            .unwrap();
        // a fleet + channel
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()])
            .unwrap();
        // plant a commander kill
        crate::cmd::commander::cmd_commander_directive(home, "dev", "kill", None, 1000).unwrap();

        // one guarded run: must stop CommanderKilled before doing any work
        let stop = run_loop_for_test(home).await;
        assert_eq!(stop, LoopStop::CommanderKilled);

        // local kill-switch clear does NOT lift the commander kill
        crate::cmd::fleet::control::cmd_fleet_start(home, "dev").ok();
        let stop2 = run_loop_for_test(home).await;
        assert_eq!(stop2, LoopStop::CommanderKilled);

        // an audit Governance entry was recorded
        let audit =
            std::fs::read_to_string(home.join("conversations").join("audit.jsonl")).unwrap();
        assert!(
            audit.contains("\"kind\":\"governance\"") && audit.contains("\"decision\":\"halted\"")
        );
    }

    #[tokio::test]
    async fn commander_zero_budget_ceiling_halts_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&cdir)
            .unwrap();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()])
            .unwrap();
        // a zero budget ceiling is a budget halt (spec §6), not a kill
        crate::cmd::commander::cmd_commander_directive(
            home,
            "dev",
            "budget_ceiling",
            Some(0.0),
            1000,
        )
        .unwrap();

        let stop = run_loop_for_test(home).await;
        assert_eq!(stop, LoopStop::Budget);

        // The audit row must bind the EXACT deciding directive — pull its real
        // nonce from the channel and assert the audit references it (guards the
        // wire-through: the loop passes gov.budget_nonce, not a constant).
        let nonce = mur_channel::ChannelService::open(home)
            .unwrap()
            .load_events("fleet-dev")
            .unwrap()
            .iter()
            .find_map(|e| {
                e.payload
                    .get("commander_directive")
                    .and_then(|d| d.get("nonce"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            })
            .expect("directive nonce present in channel");
        let audit =
            std::fs::read_to_string(home.join("conversations").join("audit.jsonl")).unwrap();
        assert!(
            audit.contains("\"decision\":\"capped\"")
                && audit.contains("\"directive\":\"budget_ceiling\"")
        );
        assert!(
            audit.contains(&format!("\"nonce\":\"{nonce}\"")),
            "audit must bind the exact deciding directive nonce ({nonce})"
        );
    }

    #[test]
    fn queue_drained_outcome_has_its_own_label() {
        // The progress file's `outcome` is how a caller tells "finished because
        // there was nothing left to do" from "ran out of iterations".
        assert_eq!(outcome_label(LoopStop::QueueDrained), "queue-drained");
        assert_ne!(
            outcome_label(LoopStop::QueueDrained),
            outcome_label(LoopStop::MaxIterations)
        );
    }

    #[tokio::test]
    async fn queue_drained_break_fires_when_policy_is_queue_empty_and_queue_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "standing goal".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: Some(mur_common::fleet::FleetLoop {
                trigger: "manual".into(),
                max_iterations: 1,
                budget_usd: 0.0,
                deadline: String::new(),
                done_when: super::super::done_policy::DONE_WHEN_QUEUE_EMPTY.into(),
            }),
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()])
            .unwrap();
        // No job is ever queued, so the break fires on iteration 1 — ahead of
        // the `plan_via_router` dial, so no live "pm" agent is needed here.
        let stop = run_loop_for_test(home).await;
        assert_eq!(stop, LoopStop::QueueDrained);
    }

    #[tokio::test]
    async fn queue_drained_break_stays_inert_under_router_policy() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "standing goal".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: Some(mur_common::fleet::FleetLoop {
                trigger: "manual".into(),
                max_iterations: 1,
                budget_usd: 0.0,
                deadline: String::new(),
                done_when: String::new(), // router policy — the fallback for empty/legacy values
            }),
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()])
            .unwrap();
        // Same empty queue as the fires case, but router policy: the gate must
        // not mistake an empty queue for `done_when: queue-empty`. No live
        // "pm" agent exists to dial, so the iteration errors out and
        // `run_loop_for_test` folds that into MaxIterations — the only claim
        // under test is that the gate did NOT mistake this for a drained queue.
        let stop = run_loop_for_test(home).await;
        assert_ne!(stop, LoopStop::QueueDrained);
    }

    #[tokio::test]
    async fn queue_empty_policy_with_claimed_job_does_not_converge_via_router() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "standing goal".into(),
            router: None,
            members: vec!["pm".into()],
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: Some(mur_common::fleet::FleetLoop {
                trigger: "manual".into(),
                max_iterations: 1,
                budget_usd: 0.0,
                deadline: String::new(),
                done_when: super::super::done_policy::DONE_WHEN_QUEUE_EMPTY.into(),
            }),
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home)
            .unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()])
            .unwrap();
        // A claimed job means `active_job` is `Some`, so the drained-queue
        // break above does NOT fire and this iteration takes the normal
        // delegate path — regression coverage for the bug where `queue-empty`
        // fell through `done_marker` (which only recognises `marker:`) into
        // `ask_router_done`, paying for an LLM call on every iteration that
        // actually does work, on a policy that promises never to make one.
        super::super::jobs::enqueue_job(home, "dev", "job-1", "cli").unwrap();
        let stop = run_loop_for_test(home).await;
        assert_ne!(stop, LoopStop::Converged);
    }
}
