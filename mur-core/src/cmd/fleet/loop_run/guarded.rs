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
    let mut fleet = store::load_fleet(mur_home, name)?;
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

    if max_iterations.is_some() {
        println!(
            "  ℹ --max-iterations is ignored since 2.79 — the bounds are deadline / stuck / cost_usd (mur fleet limits {name})"
        );
    }
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
    let deadline = Some(bounds.deadline);
    println!(
        "  bounds: deadline {} ← {}; stuck {}",
        crate::cmd::limits::fmt_dur(bounds.deadline),
        bounds.deadline_source.label(),
        match bounds.stuck {
            mur_common::limits::Stuck::Off => "off".to_string(),
            mur_common::limits::Stuck::After(d) => crate::cmd::limits::fmt_dur(d),
        }
    );
    let billing = super::super::billing::fleet_billing(mur_home, &fleet);
    let configured_budget = bounds.cost_usd.filter(|&b| b > 0.0);
    let budget = budget_for(configured_budget, &billing);
    if configured_budget.is_some_and(|b| b > 0.0) && budget.is_none() {
        println!(
            "  ℹ budget_usd ignored — fleet '{name}' runs on local/subscription models and cannot spend; \
             its bound is the deadline"
        );
    }
    if !billing.unknown.is_empty() {
        let who: Vec<String> = billing
            .unknown
            .iter()
            .map(|(a, m)| format!("{a} ({m})"))
            .collect();
        println!(
            "  ⚠ billing unknown for {} — treated as metered. Mark a local model with `billing: local` in models.yaml",
            who.join(", ")
        );
    }
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
    if let Some(line) = no_cap_notice(&billing, budget, deadline, price_per_1k) {
        println!("  {line}");
    }
    // Forward estimate before any real data (and the fail-safe fallback when an
    // iteration reports no token usage), so spend can never silently under-count.
    let projection = estimate_iteration_cost_usd(fleet.members.len(), price_per_1k);
    // Real cumulative spend, accumulated from each iteration's actual token usage.
    let mut spent = 0.0_f64;
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
    let mut last_progress = Instant::now();

    // Load commander keys once; empty = governance inert.
    let commander_keys = crate::cmd::commander::accepted_pubkeys(mur_home);
    let governed = !commander_keys.is_empty();

    // ── Run progress (deep-research UX): one best-effort JSON the run output +
    // `mur deep-research` panel render from. Every write is best-effort — a
    // failure must never fail, slow, or change the loop (see RunProgress::save).
    // The loop's handle: minted by the caller that will poll it, else fresh.
    let run_id = match run_id {
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
        iteration: 0,
        model: fleet
            .members
            .first()
            .and_then(|m| mur_common::agent::AgentProfile::load(mur_home, m).ok())
            .and_then(|p| p.model_ref),
        budget_usd: budget,
        spend_usd: 0.0,
        billable: Some(billing.billable),
        steps: vec![],
        artifact_path: None,
        error: None,
    }));
    lock_progress(&progress).save(mur_home, name);

    // The loop is one run with a handle (spec §3.6): recorded now so
    // `mur_job_status <run_id>` answers while it runs, beaten every interval,
    // and closed with the state the stop implies. Best-effort like
    // progress.json — a ledger failure never stops the loop. No sidecar: the
    // parent has no single first channel seq; `mur fleet status` keeps
    // finding the per-iteration runs through theirs.
    let runs_cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml")).runs;
    let now = chrono::Utc::now();
    let record = crate::run_status::RunState {
        schema: crate::run_status::RUN_SCHEMA,
        run_id: run_id.clone(),
        channel_id: Some(fleet.channel_id.clone()),
        kind: crate::run_status::RunKind::Fleet,
        label: format!("fleet {name} loop"),
        pid: std::process::id(),
        started_at: now,
        last_heartbeat_at: Some(now),
        state: crate::run_status::State::Running,
        steps: vec![],
        blocked_on: None,
        binary_version: env!("CARGO_PKG_VERSION").to_string(),
        build_sha: mur_common::build::SHORT_SHA.to_string(),
    };
    let loop_beat = match crate::run_status::store::save(mur_home, &record) {
        Ok(()) => Some(crate::run_status::heartbeat::Heartbeat::spawn(
            mur_home.to_path_buf(),
            run_id.clone(),
            std::time::Duration::from_secs(runs_cfg.heartbeat_interval_secs),
        )),
        Err(error) => {
            tracing::warn!(run_id = %run_id, %error, "fleet loop: run record not written; mur_job_status will not see this loop");
            None
        }
    };

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
    if triage_cfg.enabled {
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
        if super::super::control::is_stopped(mur_home, name) {
            break LoopStop::Stopped;
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
        // Static `procedure:` first (never falls back); else router, else broadcast.
        let proc = match super::super::plan::static_procedure(&planning_fleet, &dispatch_goal)? {
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
                StepEventKind::Blocked => {
                    // Waiting on a human. Not Running (nothing is executing)
                    // and not Failed (nothing went wrong): park it back at
                    // Pending, which is what it is — work still to do — and
                    // say so on its own line so the rail is not silent about
                    // why the fleet stopped short.
                    sp.state = StepState::Pending;
                    println!("⏸ {} awaiting approval", sp.id);
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
            // Fleet-declared approval policy (`hitl.mode`); `None` keeps the
            // TTY-derived default. Tightening only — nothing here approves.
            hitl_unanswered: fleet.hitl.as_ref().and_then(|h| h.mode),
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
            deadline_at: Some(start + bounds.deadline),
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
            last_progress = Instant::now();
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
    let run_id = {
        let mut g = lock_progress(&progress);
        g.finished_at = Some(chrono::Utc::now().to_rfc3339());
        g.outcome = Some(outcome_label(stop).to_string());
        g.iteration = iteration;
        g.spend_usd = spent;
        g.save(mur_home, name);
        g.run_id.clone()
    };
    // And onto the channel, where the rail, murmur and the Hub are looking.
    // Until this existed the reason lived only in progress.json, and every
    // surface said "finished" for a run that had hit a cap. Signed as the
    // writer like every other event this run wrote; best-effort like the
    // progress file — a stop must never fail because its announcement did.
    emit_stop_event(&svc, mur_home, &fleet, stop, iteration, spent, &run_id);
    // Stop the beat BEFORE the terminal write so a late tick cannot
    // resurrect `running` (see Heartbeat::stop).
    if let Some(b) = loop_beat {
        b.stop().await;
    }
    let terminal = loop_terminal_state(stop);
    if let Err(error) = crate::run_status::store::update(mur_home, &run_id, |r| r.state = terminal)
    {
        tracing::warn!(run_id = %run_id, %error, "fleet loop: terminal state not recorded");
    }

    // ── Close the triage loop ──────────────────────────────────────────────
    // The loop path carries the best evidence in the codebase: a real
    // cumulative `spent` and a stop reason the guards themselves produced.
    // `run_id` matches the gate call above, which is what pairs the halves.
    if triage_cfg.enabled {
        crate::executor::triage_gate::record_outcome(
            mur_home,
            &run_id,
            Some(spent),
            start.elapsed(),
            calibration_stop_reason(stop).map(str::to_string),
            matches!(stop, LoopStop::Converged | LoopStop::QueueDrained),
        );
    }

    Ok((stop, iteration, spent))
}

/// One System `state-change` carrying why the loop stopped and the way out.
/// `from` is always `working`: a loop that is ending was running.
fn emit_stop_event(
    svc: &mur_channel::ChannelService,
    mur_home: &Path,
    fleet: &Fleet,
    stop: LoopStop,
    iterations: u32,
    spent_usd: f64,
    run_id: &str,
) {
    let payload = serde_json::json!({
        "from": "working",
        "to": terminal_state_for(stop),
        "stop_reason": outcome_label(stop),
        "remedy": stop_remedy(stop, &fleet.name),
        "iterations": iterations,
        "spent_usd": spent_usd,
        "run_id": run_id,
    });
    if let Err(e) = crate::channel_writer::append_as_writer(
        svc,
        mur_home,
        &fleet.channel_id,
        fleet.router_or_concierge(),
        ChannelActor::System,
        mur_common::channel::EventKind::StateChange,
        payload,
        None,
    ) {
        tracing::warn!(fleet = %fleet.name, error = %e, "could not write the stop reason to the channel");
    }
}
