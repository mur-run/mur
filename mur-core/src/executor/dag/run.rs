use super::*;

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
    // Snapshot before a single event is written, so the closing summary can
    // tell the user how much of this run's history is missing. See
    // `channel_writer::refusals_since`.
    let refusal_mark = crate::channel_writer::refusal_count();

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
        // Signed like every other event this run writes — see
        // `channel_writer::transition_as_writer`.
        let _ = crate::channel_writer::transition_as_writer(
            &svc,
            mur_home,
            cid,
            ROUTER_AGENT,
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
    let emit_final = |outcome: RunOutcome| {
        // Blocked is NOT terminal: the channel is already `InputRequired`
        // (the gate put it there) and must stay that way, or the surfaces that
        // read channel state would show a finished run while a human still
        // owes it an answer.
        let st = match outcome {
            RunOutcome::Blocked => return,
            RunOutcome::Failed => ChannelState::Failed,
            RunOutcome::Done => ChannelState::Completed,
        };
        if let Some(cid) = opts.channel_id.as_deref() {
            let _ = ChannelService::open(mur_home).and_then(|svc| {
                crate::channel_writer::transition_as_writer(
                    &svc,
                    mur_home,
                    cid,
                    ROUTER_AGENT,
                    st,
                    ChannelActor::System,
                    opts.event_run_id(),
                )
            });
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
    // Step ids waiting on a human, plus everything downstream of them. A
    // dependent must not run on a prerequisite that has not happened yet —
    // but unlike a failure this is not an abort: independent branches in the
    // same and later ranks still run to completion, so an unattended run gets
    // as far as it legitimately can before it needs someone.
    let mut blocked_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for rank in 0..=max_rank {
        let indices: Vec<usize> = (0..graph.nodes.len())
            .filter(|i| graph.nodes[*i].rank == rank)
            .collect();

        if indices.is_empty() {
            continue;
        }

        // Inherit blocked-ness before spawning: a step whose dependency is
        // waiting on a human cannot run. Ranks execute in dependency order, so
        // checking direct `depends_on` here transitively covers the subgraph —
        // each rank marks the next.
        let (indices, deferred_indices): (Vec<usize>, Vec<usize>) =
            indices.into_iter().partition(|i| {
                !graph.nodes[*i]
                    .step
                    .depends_on
                    .iter()
                    .any(|d| blocked_ids.contains(d))
            });
        for i in deferred_indices {
            let step = &graph.nodes[i].step;
            let sid = step.id.clone().unwrap_or_else(|| format!("step{i}"));
            eprintln!("  Step {sid}: not run — depends on a step awaiting approval");
            if let Some(cb) = composed_on_step.as_ref() {
                cb(StepEvent {
                    id: sid.clone(),
                    agent: step.delegate_to.clone(),
                    kind: StepEventKind::Blocked,
                    tokens_used: 0,
                    error: Some("depends on a step awaiting approval".into()),
                });
            }
            blocked_ids.insert(sid);
        }
        if indices.is_empty() {
            continue;
        }

        // Concurrent: spawn each step in this rank.
        // Extract owned values from opts for the spawned tasks.
        let opt_yes = opts.yes;
        // Resolve once per rank, not per step: the default probes the TTY,
        // and every step in a run must agree on whether a human is watching.
        let opt_hitl_tiers = opts.hitl_auto_approve_tiers.clone();
        let opt_hitl_unanswered = Some(opts.hitl_unanswered.unwrap_or_else(default_unanswered));
        let opt_input = opts.input.clone();
        let opt_env_override = opts.env_class_override.map(|s| s.to_string());
        let opt_vars = opts.variables.clone();
        let opt_dev_id = opts.device_id.clone();
        let opt_trigger = opts.trigger.to_string();
        let opt_chan_id = opts.channel_id.clone();
        let opt_run_id = opts.run_id.clone();
        let opt_deadline_at = opts.deadline_at;
        let opt_needs = opts.needs.clone();
        let opt_on_step = composed_on_step.clone();
        let mut handles = Vec::new();
        for &i in &indices {
            // Mutating the graph node (not the local clone) keeps retries
            // consistent with the augmented sub-goal.
            thread_dep_outputs(&mut graph.nodes[i].step, &completed_outputs);
            let step = graph.nodes[i].step.clone();
            let env_override = opt_env_override.clone();
            let needs = opt_needs.clone();
            let dev_id = opt_dev_id.clone();
            let inp = opt_input.clone();
            let vars = opt_vars.clone();
            let tr = opt_trigger.clone();
            let chan_id = opt_chan_id.clone();
            let run_id = opt_run_id.clone();
            let on_step = opt_on_step.clone();
            let hitl_tiers = opt_hitl_tiers.clone();
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
                    hitl_unanswered: opt_hitl_unanswered,
                    hitl_auto_approve_tiers: hitl_tiers,
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
                    // The launching clock travels into every step (§3.4).
                    deadline_at: opt_deadline_at,
                    needs,
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
                        blocked: false,
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

            // A blocked step is neither a success to record nor a failure to
            // act on: register it so dependents inherit, and skip the ledger +
            // on_failure handling entirely. It never happened; the run will
            // simply end short of it.
            if result.blocked {
                blocked_ids.insert(
                    step.id
                        .clone()
                        .unwrap_or_else(|| format!("step{}", indices[ri])),
                );
                continue;
            }

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
                        emit_final(RunOutcome::Failed);
                        finalize_run(
                            mur_home,
                            &opts.run_id,
                            recorded.is_some(),
                            &mut heartbeat,
                            RunOutcome::Failed,
                            refusal_mark,
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
                                emit_final(RunOutcome::Failed);
                                finalize_run(
                                    mur_home,
                                    &opts.run_id,
                                    recorded.is_some(),
                                    &mut heartbeat,
                                    RunOutcome::Failed,
                                    refusal_mark,
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
    // Blocked outranks a clean exit code: every blocked step exits 0 (nothing
    // ran, nothing failed), so reading the exit code alone would report a run
    // waiting on a human as a success.
    let outcome = if overall_exit_code != 0 {
        RunOutcome::Failed
    } else if !blocked_ids.is_empty() {
        RunOutcome::Blocked
    } else {
        RunOutcome::Done
    };
    let status = match outcome {
        RunOutcome::Done => PipelineStatus::Success,
        RunOutcome::Failed => PipelineStatus::Failed,
        // ponytail: `Skipped` is the closest existing variant — it does not
        // claim success and needs no change to a serialized enum. Add a
        // `Blocked` variant if a consumer ever needs to tell the two apart;
        // the run record (State::Blocked) and the channel (InputRequired)
        // already carry the precise state.
        RunOutcome::Blocked => PipelineStatus::Skipped,
    };
    if outcome == RunOutcome::Blocked {
        let mut ids: Vec<&String> = blocked_ids.iter().collect();
        ids.sort();
        eprintln!(
            "\n⏸ Run stopped: {} step(s) awaiting approval ({}).\n   Approve with `mur channel approve <channel_id> <hitl_id>` (or the Hub's Needs You card), then re-run — completed steps are skipped.",
            ids.len(),
            ids.iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    emit_final(outcome);
    finalize_run(
        mur_home,
        &opts.run_id,
        recorded.is_some(),
        &mut heartbeat,
        outcome,
        refusal_mark,
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
pub(super) async fn finalize_run(
    mur_home: &std::path::Path,
    run_id: &str,
    recorded: bool,
    heartbeat: &mut Option<crate::run_status::heartbeat::Heartbeat>,
    outcome: RunOutcome,
    refusal_mark: usize,
) {
    // Before anything else, and regardless of whether this run is recorded:
    // a run whose events were refused must say so out loud. An unrecorded
    // run is the case where the channel is the ONLY history there is, so
    // skipping the warning there would silence it exactly where it matters
    // most.
    let refused = crate::channel_writer::refusals_since(refusal_mark);
    if refused > 0 {
        eprintln!(
            "\n⚠ {refused} channel write(s) refused during this run — the channel's history is INCOMPLETE.\n   Writes are refused when MUR_CHANNEL_REQUIRE_SIG is set and the signing key cannot be read.\n   Details: {}",
            mur_home
                .join("channels")
                .join("write-refusals.jsonl")
                .display()
        );
    }
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
        record.state = match outcome {
            RunOutcome::Failed => crate::run_status::State::Failed,
            RunOutcome::Blocked => crate::run_status::State::Blocked,
            RunOutcome::Done => crate::run_status::State::Done,
        };
    });
}

// ── Tests ───────────────────────────────────────────────────────────────────
