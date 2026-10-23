use super::*;

// ── Step execution ──────────────────────────────────────────────────────────

pub(super) struct StepResult {
    pub(super) exit_code: i32,
    pub(super) output_text: String,
    pub(super) duration_ms: u64,
    pub(super) failed_step: Option<String>,
    pub(super) success: bool,
    /// The step did not run and did not fail: it is waiting on a human. Kept
    /// distinct from `success` because a blocked step must neither trigger
    /// `on_failure` handling nor let dependents proceed as if it had run.
    pub(super) blocked: bool,
    /// The kill-switch halted this step (#2036): it never started, or its
    /// member turn was cancelled. Like `blocked`, never a failure to act on.
    pub(super) stopped: bool,
    /// Real LLM tokens this step consumed, input and output kept apart so a
    /// cost guard can price each side at its own rate — non-zero only for
    /// delegate steps, read from the specialist's `Task.usage`. Its total is
    /// summed into the run's `PipelineOutput.tokens_used`.
    pub(super) usage: TokenUsage,
}

/// Core step execution: run the command, handle approval, return result.
pub(super) async fn execute_step_inner(
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
                            blocked: false,
                            stopped: false,
                            usage: TokenUsage::default(),
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
                            blocked: false,
                            stopped: false,
                            usage: TokenUsage::default(),
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
                            blocked: false,
                            stopped: false,
                            usage: TokenUsage::default(),
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
            blocked: false,
            stopped: false,
            usage: TokenUsage::default(),
        }
    } else {
        // ── Intent-mode (no command) ──
        // A delegate step lands here only without a channel; say so on the
        // line itself so the output does not read as if the member ran.
        let not_delegated = if is_undelegated(step, opts) {
            format!(
                " [not delegated to {}: no channel]",
                step.delegate_to.as_deref().unwrap_or_default()
            )
        } else {
            String::new()
        };
        eprintln!(
            "  Step {}: {} {}{not_delegated}",
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
            blocked: false,
            stopped: false,
            usage: TokenUsage::default(),
        }
    }
}

// ── Channel emit helper ─────────────────────────────────────────────────────

/// Fire-and-forget: open the channel, append one System event SIGNED by the
/// router (`"mur"`, falling back to unsigned when no identity), ignore errors.
pub(super) fn emit_channel(
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
pub(super) async fn execute_step(
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
    let emit = |kind: StepEventKind, usage: TokenUsage, error: Option<String>| {
        if let Some(cb) = &opts.on_step {
            cb(StepEvent {
                id: sid.clone(),
                agent: observer_agent.clone(),
                kind,
                usage,
                error,
            });
        }
    };
    // Kill-switch at the step boundary (#2036): a step not yet started when
    // the operator stopped the run never starts — not even its Started event,
    // so `mur job status` does not show it running.
    if stop_requested(opts) {
        emit(
            StepEventKind::Stopped,
            TokenUsage::default(),
            Some(halted_reason_text(opts).into()),
        );
        return stopped_result(start.elapsed().as_millis() as u64);
    }
    emit(StepEventKind::Started, TokenUsage::default(), None);
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
            emit(StepEventKind::Done, TokenUsage::default(), None);
            return StepResult {
                exit_code: 0,
                output_text: String::new(),
                duration_ms: 0,
                failed_step: None,
                success: true,
                blocked: false,
                stopped: false,
                usage: TokenUsage::default(),
            };
        }
    }

    // ── Delegation (v3d-2): dial a specialist via `channel/delegate`; the
    // specialist runs the turn AND writes+signs its own reply Message ──
    if let (Some(target), Some(cid)) = (step.delegate_to.as_deref(), opts.channel_id.as_deref()) {
        let start = std::time::Instant::now();
        let canonical = crate::a2a_dial::canonicalize_agent_name(mur_home, target);
        let child_task_id = format!("ct-{}", uuid::Uuid::now_v7());
        // Each dispatch gets its own delegate/reply keys, discriminated by the
        // fresh child task id. `(run_id, step_id, attempt)` alone repeats when a
        // step is dispatched again under the same run (crash-rerun / resume of a
        // step with no recorded ToolResult): the dedup-aware writer would drop
        // the new Delegation and the specialist's new reply. The `result` key
        // stays deterministic so the resume cursor above still finds it.
        let deleg_key = idem_key(
            cid,
            &opts.run_id,
            &sid,
            &format!("{}#{child_task_id}", key("delegate")),
        );
        let reply_key = idem_key(
            cid,
            &opts.run_id,
            &sid,
            &format!("{}#{child_task_id}", key("reply")),
        );

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
        let params = build_channel_delegate_params(
            &goal_text,
            cid,
            &child_task_id,
            &reply_key,
            remaining_secs(opts.deadline_at, std::time::Instant::now()),
            &opts.needs,
        );
        let watch = watch_signal(opts);
        let dial = match dial_delegate_watched(
            mur_home,
            &canonical,
            &child_task_id,
            params,
            watch.as_ref(),
        )
        .await
        {
            DelegateDial::Replied(r) => r,
            DelegateDial::Stopped => {
                // Say so in the channel: the member's reply will never land,
                // and a reader should not wonder why the delegation dangles.
                if let Ok(svc) = ChannelService::open(mur_home) {
                    let _ = crate::channel_writer::append_as_writer(
                        &svc,
                        mur_home,
                        cid,
                        ROUTER_AGENT,
                        ChannelActor::System,
                        mur_common::channel::EventKind::Note,
                        serde_json::json!({
                            "text": format!("delegate to {canonical} cancelled: {}", halted_reason_text(opts))
                        }),
                        None,
                    );
                }
                let why = halted_reason_text(opts);
                eprintln!("  Step {sid}: delegate to {canonical} cancelled — {why}");
                emit(
                    StepEventKind::Stopped,
                    TokenUsage::default(),
                    Some(why.into()),
                );
                return stopped_result(start.elapsed().as_millis() as u64);
            }
        };

        let result = match dial {
            Ok(task) => {
                delegate_result(&task, &step.description, start.elapsed().as_millis() as u64)
            }
            Err(e) => {
                // No reply text is attributed; record a failure Note + fail the
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
                // The member may have run (and billed) the turn even though
                // its reply never reached us: ask it what the turn spent.
                let usage =
                    super::stop::recover_delegate_usage(mur_home, &canonical, &child_task_id).await;
                StepResult {
                    exit_code: 1,
                    output_text: format!("delegate failed: {e}"),
                    duration_ms: start.elapsed().as_millis() as u64,
                    failed_step: Some(step.description.clone()),
                    success: false,
                    blocked: false,
                    stopped: false,
                    usage,
                }
            }
        };
        emit(
            if result.success {
                StepEventKind::Done
            } else {
                StepEventKind::Failed
            },
            result.usage.clone(),
            step_failure_reason(&result),
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
            // The gate is only reached on the LOCAL path — delegate steps return
            // above — so the asker is the router itself, not the step's agent.
            agent_id: ROUTER_AGENT.into(),
            summary: step.description.clone(),
        };
        let decision = crate::hitl::gate::gate(
            mur_home,
            cid,
            &req,
            &crate::hitl::gate::GatePolicy {
                yes: opts.yes,
                unanswered: opts.hitl_unanswered.unwrap_or_else(default_unanswered),
                auto_approve_tiers: opts.hitl_auto_approve_tiers.clone(),
            },
            None,
            Some(opts.run_id.as_str()),
        )
        .await
        .unwrap_or(crate::hitl::gate::GateDecision {
            allow: false,
            deferred: false,
            reason: "gate error".into(),
            action_hash: String::new(),
            hitl_id: None,
        });
        if decision.deferred {
            // Parked, not refused: the request is durable in the channel and
            // any surface can answer it later. Report blocked so the run stops
            // here without burning the wait window or failing work a human
            // never got to see.
            eprintln!(
                "  Step {sid}: awaiting approval — {} (approve: mur channel approve {cid} <hitl_id>)",
                decision.reason
            );
            emit(StepEventKind::Blocked, TokenUsage::default(), None);
            return StepResult {
                exit_code: 0,
                output_text: decision.reason.clone(),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: None,
                success: false,
                blocked: true,
                stopped: false,
                usage: TokenUsage::default(),
            };
        }
        if !decision.allow {
            eprintln!("  Step {sid}: gate denied ({})", decision.reason);
            emit(
                StepEventKind::Failed,
                TokenUsage::default(),
                Some(format!("hitl: {}", decision.reason)),
            );
            return StepResult {
                exit_code: 1,
                output_text: format!("hitl: {}", decision.reason),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: Some(step.description.clone()),
                success: false,
                blocked: false,
                stopped: false,
                usage: TokenUsage::default(),
            };
        }
        // Re-verify the pin at the execute boundary (fail-closed on drift).
        let now_hash = crate::hitl::pin::action_hash("sh", &input, cid, &sid, "mur");
        if !decision.action_hash.is_empty() && now_hash != decision.action_hash {
            eprintln!("  Step {sid}: hitl_drift at execute boundary — refusing");
            emit(
                StepEventKind::Failed,
                TokenUsage::default(),
                Some("hitl_drift".into()),
            );
            return StepResult {
                exit_code: 1,
                output_text: "hitl_drift".into(),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: Some(step.description.clone()),
                success: false,
                blocked: false,
                stopped: false,
                usage: TokenUsage::default(),
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
            // NOT success. Reporting an unapproved step as done let every
            // dependent run on a prerequisite that never happened, and the run
            // as a whole reported success — the same silent-skip-as-success
            // shape the audit found elsewhere. Unattended (no TTY) it is
            // blocked; interactively it is a refusal the human just gave.
            let refused = std::io::IsTerminal::is_terminal(&std::io::stdin());
            eprintln!(
                "  Step {sid}: needs_approval and {} — not run. Re-run interactively, or with `--yes` to auto-approve.",
                if refused { "declined" } else { "nobody to ask" }
            );
            emit(
                if refused {
                    StepEventKind::Failed
                } else {
                    StepEventKind::Blocked
                },
                TokenUsage::default(),
                refused.then(|| "needs_approval: declined".to_string()),
            );
            return StepResult {
                exit_code: if refused { 1 } else { 0 },
                output_text: "needs_approval: not approved".into(),
                duration_ms: 0,
                failed_step: refused.then(|| step.description.clone()),
                success: false,
                blocked: !refused,
                stopped: false,
                usage: TokenUsage::default(),
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
        let excerpt = truncate_chars(&result.output_text, CHANNEL_EXCERPT_MAX_CHARS);
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

    if result.success && is_undelegated(step, opts) {
        emit(
            StepEventKind::Skipped,
            TokenUsage::default(),
            Some(UNDELEGATED_REASON.into()),
        );
    } else {
        emit(
            if result.success {
                StepEventKind::Done
            } else {
                StepEventKind::Failed
            },
            result.usage.clone(),
            step_failure_reason(&result),
        );
    }

    result
}

/// Recorded on a step that [`is_undelegated`]: the reason `mur job status`
/// and `mur_job_status` print under its `skipped` row.
pub(super) const UNDELEGATED_REASON: &str =
    "not delegated: run has no channel (re-run with --channel-new or --channel <id>)";

/// A `delegate_to` step with no command, run without a channel: it falls
/// through to intent mode, prints its description and calls no member. It
/// still reports success (a preview is legitimate), but the record must not
/// say `done` for work nobody did (#1613).
pub(super) fn is_undelegated(step: &ProcedureStep, opts: &DagExecOptions<'_>) -> bool {
    step.delegate_to.is_some() && opts.channel_id.is_none() && step.command.is_none()
}

/// The result of a step the kill-switch halted. Exit 0 and not a success: it
/// neither ran to completion nor failed, so it must trigger no `on_failure`
/// handling and feed no dependent.
pub(super) fn stopped_result(duration_ms: u64) -> StepResult {
    StepResult {
        exit_code: 0,
        output_text: String::new(),
        duration_ms,
        failed_step: None,
        success: false,
        blocked: false,
        stopped: true,
        usage: TokenUsage::default(),
    }
}
