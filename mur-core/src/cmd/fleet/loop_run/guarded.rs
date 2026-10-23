use std::ops::ControlFlow;

use super::guarded_steps::{commander_gate, finish_run};
use super::pause::refuse_pause;
use super::preflight::{SpendPlan, spend_plan};
use super::*;

/// Inner guarded loop: runs iterations until a stop reason fires. Returns
/// `(stop, iterations_completed, spent_usd)`. Extracted so tests can call it
/// directly and inspect the `LoopStop` without going through the print layer.
///
/// `goal_override` is this run's goal in place of `fleet.yaml`'s standing one.
/// It replaces the goal on the in-memory copy only, so every reader below —
/// the progress record, triage, the iteration fallback, the router's done
/// check — sees the same text, and `fleet.yaml` is never written. A run writes
/// run state, never the fleet's definition: an agent-triggered run cannot
/// write `fleets/` at all.
///
/// `cwd` is where the work is (#1607). `Some` routes every iteration's
/// members to it and runs the write-grant gate ONCE, before the first
/// iteration — a blocked member stops the run before anything is sent.
/// `None` means the run has no target tree (research, a daemon tick with no
/// directory configured): nothing is routed and nothing is gated, as before.
#[allow(clippy::too_many_arguments)]
pub async fn run_guarded(
    mur_home: &Path,
    name: &str,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    run_id: Option<String>,
    goal_override: Option<String>,
    cwd: Option<RunCwd>,
) -> Result<(LoopStop, u32, f64)> {
    if max_iterations.is_some() {
        println!(
            "  ℹ --max-iterations is ignored since 2.79 — the bounds are deadline / stuck / cost_usd (mur fleet limits {name})"
        );
    }
    run_guarded_inner(
        mur_home,
        name,
        deadline,
        budget_usd,
        run_id,
        goal_override,
        cwd,
        None,
    )
    .await
}

/// [`run_guarded`] that may continue a paused run instead of starting one
/// (pause/resume design §5.4). `resume: None` is a fresh run.
pub(crate) async fn run_guarded_from(
    mur_home: &Path,
    name: &str,
    goal_override: Option<String>,
    cwd: Option<RunCwd>,
    resume: Option<super::resume::ResumeFrom>,
) -> Result<(LoopStop, u32, f64)> {
    run_guarded_inner(mur_home, name, None, None, None, goal_override, cwd, resume).await
}

#[allow(clippy::too_many_arguments)]
async fn run_guarded_inner(
    mur_home: &Path,
    name: &str,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    run_id: Option<String>,
    goal_override: Option<String>,
    cwd: Option<RunCwd>,
    mut resume: Option<super::resume::ResumeFrom>,
) -> Result<(LoopStop, u32, f64)> {
    // One live run per fleet, taken before any admission check and held (via
    // the guard) until this function returns or the process dies.
    let _run_lock = run_lock::acquire(mur_home, name)?;
    // With the lock held: a fresh run over a paused one is refused (it would
    // orphan the paused run and reset its limits, design §8.4); a resume
    // re-runs admission — a stop (§6) or another resume may have landed
    // since `admit` — and must still hold the pause it admitted.
    match &resume {
        None => super::resume::refuse_fresh_run_over_pause(mur_home, name)?,
        Some(r) => super::resume::revalidate_under_lock(mur_home, name, r)?,
    }
    let mut fleet = store::load_fleet(mur_home, name)?;
    // Kept for the checkpoint: `fleet.goal` is overwritten below, and a resume
    // must carry the run-scoped goal, never `fleet.yaml`'s.
    let goal_override_text = goal_override.clone();
    let target_cwd_text = cwd
        .as_ref()
        .and_then(|c| c.path.as_ref())
        .map(|p| p.display().to_string());
    if let Some(goal) = goal_override {
        fleet.goal = goal;
    }
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

    if fleet
        .loop_cfg
        .as_ref()
        .is_some_and(|l| l.max_iterations != 0)
    {
        println!(
            "  ℹ fleet.yaml loop.max_iterations is IGNORED since 2.79 — remove it (mur fleet limits {name})"
        );
    }
    let bounds = fleet_bounds(mur_home, &fleet, deadline.as_deref(), budget_usd)?;
    let bounds = match &resume {
        Some(r) => super::resume::tighten(bounds, r),
        None => bounds,
    };
    let SpendPlan {
        billing,
        budget,
        price_per_1k,
        projection,
    } = spend_plan(mur_home, &fleet, name, &bounds);
    // Real cumulative spend, accumulated from each iteration's actual token usage.
    let mut spent = resume.as_ref().map_or(0.0, |r| r.spent_usd);
    // Resolve and gate before the clock starts, so an approval wait does not
    // eat the deadline. Gated once for the whole run: members and the target
    // are fixed for its lifetime. A queued job is claimed per iteration, so a
    // block here leaves it queued for the run after the approval.
    let route = match &cwd {
        Some(c) => {
            let work_dir = c.resolve()?;
            if let Some(msg) =
                super::super::run::gate_members(mur_home, &fleet, &work_dir, c.inferred).await?
            {
                anyhow::bail!("fleet '{name}': {msg}");
            }
            Some(routing_note(&work_dir, c.inferred))
        }
        None => None,
    };
    let start = match &resume {
        Some(r) => clock::Stopwatch::resumed(r.active_elapsed),
        None => clock::Stopwatch::fresh(),
    };
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let mut last_seq = svc
        .load_events(&fleet.channel_id)?
        .last()
        .map(|e| e.seq)
        .unwrap_or(0);
    // Baseline for the structured `done_when: marker:<TEXT>` check: only events
    // produced during THIS run (seq > start_seq) count, so a marker left in the
    // channel by a previous run can't make the loop converge instantly.
    // A resume keeps the original baseline, so a marker written before the
    // pause by this same run still counts.
    let start_seq = resume.as_ref().map_or(last_seq, |r| r.start_seq);
    let mut iteration = resume.as_ref().map_or(0, |r| r.iteration);
    // Stuck-detection is not reset by a pause: the age carries over.
    let mut last_progress = match &resume {
        Some(r) => clock::Stopwatch::resumed(r.stuck_elapsed),
        None => clock::Stopwatch::fresh(),
    };

    // Load commander keys once; empty = governance inert.
    let commander_keys = crate::cmd::commander::accepted_pubkeys(mur_home);
    let governed = !commander_keys.is_empty();

    // ── Run progress (deep-research UX): one best-effort JSON the run output +
    // `mur deep-research` panel render from. Every write is best-effort — a
    // failure must never fail, slow, or change the loop (see RunProgress::save).
    // The loop's handle: minted by the caller that will poll it, else fresh.
    let run_id = match resume.as_ref().map(|r| r.run_id.clone()).or(run_id) {
        Some(id) => {
            if !crate::run_status::valid_run_id(&id) {
                anyhow::bail!(
                    "invalid --run-id `{id}`: letters, digits, `-` and `_` only, at most 96 chars"
                );
            }
            id
        }
        None => format!("fleet-{name}-{}", uuid::Uuid::now_v7()),
    };
    let progress = Arc::new(Mutex::new(RunProgress {
        schema_version: 1,
        run_id: run_id.clone(),
        question: fleet.goal.clone(),
        started_at: chrono::Utc::now().to_rfc3339(),
        finished_at: None,
        outcome: None,
        iteration,
        model: fleet
            .members
            .first()
            .and_then(|m| mur_common::agent::AgentProfile::load(mur_home, m).ok())
            .and_then(|p| p.model_ref),
        budget_usd: budget,
        spend_usd: spent,
        billable: Some(billing.billable),
        steps: vec![],
        artifact_path: None,
        error: None,
        paused_at: None,
    }));
    lock_progress(&progress).save(mur_home, name);

    // The loop is one run with a handle (spec §3.6); see `run_record`.
    let loop_beat = run_record::open(mur_home, &fleet, name, &run_id);
    // The HITL policy this run enforces — and persists on every pause, so a
    // pause/resume cycle can never loosen it (design §8.2a).
    let hitl = super::resume::effective_hitl(
        fleet.hitl.as_ref().and_then(|h| h.mode),
        resume.as_ref().map(|r| r.hitl_unanswered),
        crate::executor::dag::default_unanswered(),
    );
    if let Some(r) = &resume {
        // Record `Running` (above), write `run_resumed`, drop the checkpoint,
        // and `.paused` LAST. Deleting the checkpoint is the one commit point:
        // every recovery path finds a run through `.paused`, so a checkpoint
        // must never outlive it (design §5.4 step 6). Before the commit a
        // `Running` record beside both reads as an interrupted resume; after
        // it, the run is an ordinary crashed run and `.paused` is a stale
        // leftover the next run or `stop` removes (§8.5). The checkpoint can
        // not wait for the first resumed iteration: after that it would roll
        // spend and elapsed time back.
        super::resume::record_resumed(&svc, mur_home, &fleet, r);
        checkpoint::remove(mur_home, &run_id);
        let _ = std::fs::remove_file(super::super::control::paused_path(mur_home, name));
    }

    // ── Pre-dispatch triage ────────────────────────────────────────────────
    // Once for the whole loop, not once per iteration: the goal does not
    // change between iterations, so re-asking would pay the model tax N times
    // for one answer and write N verdicts that all pair against one outcome.
    //
    // Off unless `triage.enabled`. In shadow mode (the default when enabled)
    // the verdict is recorded and the loop runs anyway — that is what makes
    // the prediction scoreable. See `executor::triage_gate`.
    let triage_cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
        .triage
        .clone();
    if triage_cfg.enabled && resume.is_none() {
        let g = crate::executor::triage_gate::gate(
            mur_home,
            &run_id,
            &fleet.goal,
            &bounds.resolved,
            triage_cfg.enforces(),
        )
        .await;
        if let Some(line) = g.console_line() {
            println!("{line}");
        }
        if g.blocks() {
            // Stop before the first iteration, so nothing has been spent and
            // the queued job stays queued for a human to resize. The heartbeat
            // is closed by the normal terminal path below.
            if let Some(b) = loop_beat {
                b.stop().await;
            }
            let _ = crate::run_status::store::update(mur_home, &run_id, |r| {
                r.state = crate::run_status::State::Blocked
            });
            anyhow::bail!(
                "fleet '{name}': triage held this goal back before the first iteration.\n\
                 Split it, or set `triage.enforce: false` in ~/.mur/config.yaml to run it\n\
                 anyway and record whether triage was right."
            );
        }
    }

    let stop = loop {
        // Commander governance (highest priority). Fail-closed: a channel read
        // error halts rather than running ungoverned.
        let commander_ceiling = if governed {
            match commander_gate(mur_home, name, &svc, &fleet.channel_id, &commander_keys) {
                ControlFlow::Continue(ceiling) => ceiling,
                ControlFlow::Break(stop) => break stop,
            }
        } else {
            None
        };

        // Kill-switch: a `mur fleet stop` between iterations halts here. Stop
        // is read before pause everywhere (design §6).
        if super::super::control::is_stopped(mur_home, name) {
            break LoopStop::Stopped;
        }
        // A pause taken between iterations: nothing is in flight, so the
        // iteration to re-enter is this one and no procedure is stored.
        if super::super::control::is_paused(mur_home, name, &run_id) {
            let snap = pause::PauseSnapshot {
                mur_home,
                fleet: &fleet,
                run_id: &run_id,
                iteration,
                spent_usd: spent,
                active_elapsed: start.elapsed(),
                stuck_elapsed: last_progress.elapsed(),
                start_seq,
                job_id: None,
                goal_override: goal_override_text.as_deref(),
                bounds: &bounds,
                target_cwd: target_cwd_text.as_deref(),
                procedure: &[],
                hitl,
            };
            match pause::record_pause(&svc, &snap) {
                Ok(()) => break LoopStop::Paused,
                Err(error) => refuse_pause(mur_home, &run_id, &error),
            }
        }
        if let Some(stop) = check_guards(
            iteration,
            start.elapsed(),
            bounds.deadline,
            last_progress.elapsed(),
            bounds.stuck,
        ) {
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
        // A resumed iteration reuses the job it claimed and the plan it was
        // running; it neither claims again nor asks the router again (§5.4).
        let reentry = resume.take();
        let (iter_goal, mut active_job) = match reentry.as_ref() {
            Some(r) => super::resume::reentered_goal(mur_home, name, &fleet.goal, r)?,
            None => iteration_goal(mur_home, name, &fleet.goal)?,
        };
        let stored_plan = reentry.filter(|r| !r.procedure.is_empty()).map(|r| {
            mur_common::skill::manifest::Procedure {
                variables: vec![],
                steps: r.procedure,
            }
        });

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
        // What members are sent: the goal plus the routing note when the run
        // has a target. Synthesis below keeps the bare goal — it writes no files.
        let dispatch_goal = match &route {
            Some(note) => format!("{iter_goal}{note}"),
            None => iter_goal.clone(),
        };
        let planning_fleet = mur_common::fleet::Fleet {
            goal: dispatch_goal.clone(),
            ..fleet.clone()
        };
        // The stored plan of a resumed iteration; else static `procedure:`
        // (never falls back); else router, else broadcast.
        let static_plan = match stored_plan {
            Some(p) => Some(p),
            None => super::super::plan::static_procedure(&planning_fleet, &dispatch_goal)?,
        };
        let proc = match static_plan {
            Some(p) => p,
            None => super::super::plan::plan_via_router(
                mur_home,
                &planning_fleet,
                &dispatch_goal,
                &pre_events,
            )
            .unwrap_or_else(|| {
                build_fleet_procedure(&dispatch_goal, &fleet.members, fleet.parallel.as_ref())
                    .expect("members validated by caller guard")
            }),
        };
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
        let on_step = observer::step_observer(
            progress.clone(),
            mur_home.to_path_buf(),
            name.to_string(),
            price_per_1k,
        );
        let pausable = checkpoint::procedure_fits(&proc.steps);
        if !pausable {
            refuse_pause(
                mur_home,
                &run_id,
                &format!(
                    "iteration {} plans more than the {}-byte checkpoint limit; `mur fleet stop` is the way out",
                    iteration + 1,
                    checkpoint::CHECKPOINT_PROCEDURE_MAX_BYTES
                ),
            );
        } else {
            checkpoint::clear_pause_refusal(mur_home, &run_id);
        }
        let opts = crate::executor::dag::DagExecOptions {
            // Fail-closed on the unattended loop path: never blanket-approve.
            // (No risk tier on fan-out steps today; this guards future
            // router-emitted risk steps. Best-practice audit / OWASP ASI06.)
            yes: false,
            // Fleet-declared approval policy (`hitl.mode`); `None` keeps the
            // TTY-derived default. Tightening only — nothing here approves.
            // `hitl` above: `fleet.yaml`'s mode, or on a resume the frozen
            // policy tightened by it, so a terminal resume cannot turn
            // `defer` into `wait` (§8.2a).
            hitl_unanswered: Some(hitl),
            hitl_auto_approve_tiers: fleet
                .hitl
                .as_ref()
                .map(|h| h.auto_approve_tiers.clone())
                .unwrap_or_default(),
            channel_id: Some(fleet.channel_id.clone()),
            // uuid nonce so concurrent `--loop` runs don't collide on the
            // channel's idempotency-key dedup (the iteration stays for readability).
            run_id: format!("{run_id}-{iteration}"),
            run_kind: Some(crate::run_status::RunKind::Fleet),
            run_label: format!("fleet {name} iter {iteration}"),
            on_step: Some(on_step),
            // #2036: the kill-switch reaches into the iteration — steps not
            // yet started never start, and an in-flight member turn is
            // cancelled — instead of waiting for the next iteration's check.
            stop: Some(super::super::control::stop_signal(mur_home, name)),
            // A pause reaches the in-flight iteration by the same route, but
            // only when the checkpoint could carry this iteration's plan; a
            // procedure over the cap makes the pause refused, not the run
            // unresumable (design §4).
            halt: pausable.then(|| super::super::control::halt_signal(mur_home, name, &run_id)),
            deadline_at: Some(start.deadline_at(bounds.deadline)),
            needs: fleet.needs.clone(),
            ..Default::default()
        };
        let out =
            crate::executor::dag::execute_dag(mur_home, &format!("fleet:{name}"), &proc, &opts)
                .await?;
        // A failed iteration is not a base to synthesize from. Only `Skipped`
        // was checked here before, so `Failed` fell through to the synthesis
        // turn below: the router, handed an empty evidence set, still emitted
        // the convergence marker, the loop broke `Converged`, and the run
        // recorded `done`. Observed 2026-09-20 — every deep-research member
        // failed to start, and the synthesized report's own text admitted it
        // had been given no worker evidence while the run claimed success.
        //
        // Checked BEFORE synthesis on purpose: that turn is a paid LLM call,
        // and buying a summary of nothing is how the fabricated report got
        // written in the first place.
        // #2036: the kill-switch halted this iteration. Account what it burned,
        // mark the job canceled, and leave BEFORE synthesis — that turn is
        // another paid call the operator just asked not to make.
        if out.status == mur_common::pipeline::PipelineStatus::Stopped
            && !super::super::control::is_stopped(mur_home, name)
            && super::super::control::is_paused(mur_home, name, &run_id)
        {
            // A pause, not a stop: the iteration is unfinished and will be
            // re-entered on resume, so it is NOT counted, the job stays
            // `Running` under its per-iteration run id, and real burn is still
            // charged (design §5.2). Synthesis is skipped for the same reason
            // as on a stop.
            if let Some(job) = active_job.as_mut() {
                job.run_id = Some(opts.run_id.clone());
                let _ = super::super::jobs::save_job(mur_home, name, job);
            }
            spent += iteration_cost_usd(out.tokens_used, price_per_1k);
            {
                let mut g = lock_progress(&progress);
                g.spend_usd = spent;
                g.save(mur_home, name);
                println!("{}", crate::cmd::fleet::progress::iteration_paused_line(&g));
            }
            let snap = pause::PauseSnapshot {
                mur_home,
                fleet: &fleet,
                run_id: &run_id,
                iteration,
                spent_usd: spent,
                active_elapsed: start.elapsed(),
                stuck_elapsed: last_progress.elapsed(),
                start_seq,
                job_id: active_job.as_ref().map(|j| j.id.as_str()),
                goal_override: goal_override_text.as_deref(),
                bounds: &bounds,
                target_cwd: target_cwd_text.as_deref(),
                procedure: &proc.steps,
                hitl,
            };
            match pause::record_pause(&svc, &snap) {
                Ok(()) => break LoopStop::Paused,
                Err(error) => {
                    // No checkpoint exists, so the run must not read as
                    // paused. Say so and let the operator stop it.
                    refuse_pause(mur_home, &run_id, &error);
                    break LoopStop::IterationFailed;
                }
            }
        }
        if out.status == mur_common::pipeline::PipelineStatus::Stopped {
            iteration += 1;
            if let Some(job) = active_job.as_mut() {
                job.run_id = Some(opts.run_id.clone());
                job.finished_at = Some(chrono::Utc::now().to_rfc3339());
                job.status = JobStatus::Canceled;
                job.error = Some("stopped by `mur fleet stop`".to_string());
                let _ = super::super::jobs::save_job(mur_home, name, job);
            }
            // Real usage only: a stopped iteration may have spent nothing,
            // and charging it the full projection would overstate the bill.
            spent += iteration_cost_usd(out.tokens_used, price_per_1k);
            {
                let mut g = lock_progress(&progress);
                g.spend_usd = spent;
                g.save(mur_home, name);
                println!("{}", iteration_summary_line(&g));
            }
            break LoopStop::Stopped;
        }
        if out.status == mur_common::pipeline::PipelineStatus::Failed {
            iteration += 1;
            if let Some(job) = active_job.as_mut() {
                job.run_id = Some(opts.run_id.clone());
                job.finished_at = Some(chrono::Utc::now().to_rfc3339());
                job.status = JobStatus::Failed;
                job.error = Some(
                    out.output_text
                        .clone()
                        .filter(|t| !t.trim().is_empty())
                        .unwrap_or_else(|| "iteration failed with no step output".to_string()),
                );
                let _ = super::super::jobs::save_job(mur_home, name, job);
            }
            // Account what the failed iteration actually burned — a failure
            // that spent tokens must still show up against the budget.
            spent += if out.tokens_used > 0 {
                iteration_cost_usd(out.tokens_used, price_per_1k)
            } else {
                projection
            };
            {
                let mut g = lock_progress(&progress);
                g.spend_usd = spent;
                g.save(mur_home, name);
                println!("{}", iteration_summary_line(&g));
            }
            break LoopStop::IterationFailed;
        }
        // Marker policies need an explicit router synthesis turn. Planning is
        // JSON-only, and delegated workers cannot speak for the router, so
        // without this phase no one can legitimately emit the convergence
        // sentinel and the same paid plan repeats until a guard fires.
        let done_when = fleet
            .loop_cfg
            .as_ref()
            .map(|l| l.done_when.as_str())
            .unwrap_or("");
        let synthesis_tokens = match done_policy(done_when) {
            DonePolicy::Marker(marker) => synthesize_via_router(
                mur_home,
                &fleet,
                &iter_goal,
                marker,
                out.output_text.as_deref().unwrap_or_default(),
            )
            .unwrap_or(0),
            _ => 0,
        };
        iteration += 1;
        // A blocked iteration reached an action that needs a human and stopped
        // there. Mark the job blocked (not Done — nothing finished) and leave
        // the loop: the next iteration would re-run the same steps, re-spend
        // the same tokens, and arrive at the same unanswered question. The
        // parked request stays in the channel, so approving it and re-running
        // picks up where this left off.
        let blocked = out.status == mur_common::pipeline::PipelineStatus::Skipped;
        if blocked && let Some(job) = active_job.as_mut() {
            job.run_id = Some(opts.run_id.clone());
            job.status = JobStatus::Blocked;
            job.error = Some("awaiting approval".to_string());
            let _ = super::super::jobs::save_job(mur_home, name, job);
        }
        if blocked {
            break LoopStop::AwaitingApproval;
        }
        // Terminal stamp: mark the queued job Done with the result of this iteration.
        if let Some(job) = active_job.as_mut() {
            job.run_id = Some(opts.run_id.clone());
            job.finished_at = Some(chrono::Utc::now().to_rfc3339());
            job.status = JobStatus::Done;
            job.result = out.output_text.clone().filter(|t| !t.is_empty());
            let _ = super::super::jobs::save_job(mur_home, name, job);
        }
        // Account REAL cost from this iteration's token usage. A 0-token result
        // (older runtime, stub, or a reply that carried no usage) falls back to
        // the projection so the budget guard never silently under-counts.
        let iteration_tokens = out.tokens_used.saturating_add(synthesis_tokens);
        spent += if iteration_tokens > 0 {
            iteration_cost_usd(iteration_tokens, price_per_1k)
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

        // Stuck-detection (§3.5, fleet half): an agent-authored channel event
        // is progress and resets the clock; a router-only iteration is not.
        let events = svc.load_events(&fleet.channel_id)?;
        let progressed = events
            .iter()
            .any(|e| e.seq > last_seq && matches!(e.actor, ChannelActor::Agent { .. }));
        last_seq = events.last().map(|e| e.seq).unwrap_or(last_seq);
        if progressed {
            last_progress = clock::Stopwatch::fresh();
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
        // #2036: a stop requested while this iteration ran wins over
        // convergence — otherwise a stop during the final iteration is lost
        // and the run is recorded `done`. Checked after the router ask too,
        // since that call can take a while.
        if super::super::control::is_stopped(mur_home, name) {
            break LoopStop::Stopped;
        }
        if converged {
            break LoopStop::Converged;
        }
    };

    finish_run(
        mur_home,
        name,
        &fleet,
        &svc,
        &progress,
        loop_beat,
        triage_cfg.enabled,
        &start,
        stop,
        iteration,
        spent,
    )
    .await;

    Ok((stop, iteration, spent))
}
