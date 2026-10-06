use super::*;

impl TaskRunner {
    /// Run the agentic loop. Returns the final agent message plus an optional
    /// `LoopStop` describing which budget (if any) forced an early, graceful
    /// exit. `None` means the model ended the turn naturally.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_agentic_loop(
        &self,
        task_id: &str,
        client: &dyn crate::llm::LlmClient,
        system_prompt: String,
        input: &Message,
        context_task_id: Option<&str>,
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
        mut steer_rx: Option<tokio::sync::mpsc::Receiver<String>>,
        intent: RequestIntent,
        bounds: crate::bounds::TurnBounds,
    ) -> Result<(Message, Option<LoopExit>), TaskError> {
        use crate::llm::{LlmRequest, RichMessage, StopReason};

        // `suggest_replies` is offered to the model only on streaming
        // (interactive) turns — non-interactive callers never see it.
        let streaming = sink.is_some();
        // Tools a gate refused this turn (spec §3.8): told once, then not
        // offered again, so the model cannot spin on "not authorized" ×3.
        let mut disabled: HashSet<String> = HashSet::new();
        /// A withdrawn tool answers every call the same way without running
        /// anything, so repeated calls are pure burn. Counted across the turn,
        /// not per-fingerprint: varying the arguments is exactly what defeats
        /// the doom-loop detector here.
        const WITHDRAWN_CALL_LIMIT: usize = 3;
        /// What the runtime says when it carries a turn onward under
        /// `Autonomy::Continue`. Deliberately an INSTRUCTION TO RE-CHECK, not
        /// an order to invent more work: a model with genuinely nothing left
        /// must be able to end the turn a second time, and that second
        /// `EndTurn` settles because the budget is spent.
        const CONTINUE_NUDGE: &str = "You have standing authorization to keep going \
             (autonomy: continue). If work from this task remains unfinished, continue it \
             now without asking. If everything is genuinely done, or the next step needs a \
             decision only the user can make, say so plainly and end the turn.";
        let mut withdrawn_calls = 0usize;
        // Seed with prior conversation threaded via `context.task_id` so the
        // model has multi-turn memory; this turn's tool scaffolding is appended
        // below and stays ephemeral (never persisted into chat memory).
        // The pinned block is rendered here, once, before the loop: every
        // step resends this same list, so it rides at index 1 throughout.
        let (pinned, prior) = self.pinned_and_prior(task_id, context_task_id);
        let mut history: Vec<RichMessage> = seed_history(system_prompt, pinned, prior, input);

        // Rolling window of recent tool-call fingerprints for doom-loop
        // detection: (tool_name, hash(canonical args), hash(result content)).
        // Keying on the RESULT too means a command repeated with identical
        // output (genuinely stuck) trips the guard, while the same command
        // returning changing output (e.g. `cargo build` between edits) does
        // NOT — that's progress, not a loop. Requires fingerprinting AFTER
        // the tool runs, since the result isn't known until then.
        let mut fingerprints: VecDeque<(String, u64, u64)> = VecDeque::with_capacity(LOOP_WINDOW);
        // Settlement accounting for this turn. Recorded from the loop's own
        // view of each call, so the card cannot disagree with what happened.
        let mut ledger = crate::turn_ledger::TurnLedger {
            agent: self.agent_name.clone(),
            ..Default::default()
        };
        // The stuck clock (§3.5). The last three calls travel in the stop
        // reason; the ledger travels in the card. Attended turns are warned
        // once — there is no second warning because the person is the stop.
        let mut progress = crate::bounds::Progress::start(std::time::Instant::now());
        let mut stuck_warned = false;

        // Issue #001 continuation state. `gate_blocked` latches for the whole
        // turn rather than resetting per iteration: a gate that parked an
        // action is a human owed an answer, and that debt does not expire
        // because a later, unrelated tool call happened to succeed.
        let mut continuations_used: u32 = 0;
        let mut gate_blocked = false;

        // Text the user has already been shown this turn, one entry per model
        // call. Only streaming turns show intermediate text; a non-streaming
        // caller sees nothing until the reply, so there is nothing to keep.
        let mut shown: Vec<String> = Vec::new();
        // The previous model call's only tools were `suggest_replies`. That
        // tool is a no-op, so the model has usually said all it will; an empty
        // answer to its tool result is the model ending the turn, not a blip.
        let mut after_suggest_only = false;
        // Text streamed before a `suggest_replies`-only call. No step card
        // freezes it on screen, so the CLI still holds it in the same bubble
        // the final reply replaces; the reply must carry it or it is erased.
        let mut carried: Vec<String> = Vec::new();
        // The bubble on screen already holds text from an earlier model call
        // this turn, with no step card after it. The next call streams into
        // the same bubble, so it must open with the same `"\n\n"` the reply
        // joins the segments with — otherwise the streamed text and the reply
        // differ, and the CLI can no longer tell which part of the reply it
        // has already committed to scrollback (it re-printed the whole reply
        // under the part already there).
        let mut bubble_has_text = false;

        let mut iteration: u32 = 0;
        while iteration < self.iteration_ceiling {
            let tool_defs: Vec<_> = self
                .tools_for_loop()
                .iter()
                .map(|t| t.def())
                .filter(|d| crate::tools::suggest::offer_for_streaming(&d.name, streaming))
                .filter(|d| !disabled.contains(&d.name))
                .collect();
            let now = std::time::Instant::now();
            if let Some(d) = bounds.deadline
                && now >= d
            {
                self.kill_jobs_of(task_id).await;
                let msg = self
                    .graceful_exit(
                        task_id,
                        client,
                        &history,
                        LoopStop::Deadline,
                        &ledger,
                        iteration,
                        &progress,
                    )
                    .await;
                return Ok((
                    msg,
                    Some(LoopExit {
                        reason: LoopStop::Deadline,
                        iterations: iteration,
                    }),
                ));
            }
            if let mur_common::limits::Stuck::After(limit) = bounds.stuck
                && iteration > 0
                && progress.stuck_for(now) >= limit
            {
                if bounds.attended {
                    if !stuck_warned {
                        stuck_warned = true;
                        self.emit_live_warning(
                            task_id,
                            &format!(
                                "⚠ no progress for {} — Esc to stop",
                                crate::bounds::fmt_dur(limit)
                            ),
                        )
                        .await;
                    }
                } else {
                    self.kill_jobs_of(task_id).await;
                    let msg = self
                        .graceful_exit(
                            task_id,
                            client,
                            &history,
                            LoopStop::Stuck,
                            &ledger,
                            iteration,
                            &progress,
                        )
                        .await;
                    return Ok((
                        msg,
                        Some(LoopExit {
                            reason: LoopStop::Stuck,
                            iterations: iteration,
                        }),
                    ));
                }
            }
            let req = LlmRequest {
                messages: history.clone(),
                temperature: None,
                max_tokens: None,
                effort: self.effort(),
                tools: tool_defs.clone(),
                intent,
                task_id: Some(task_id.to_string()),
                ..Default::default()
            };
            // Bounded retry for a transient empty-stream hiccup: an
            // `InvalidResponse` carrying "empty streamed response" is usually a
            // momentary network/proxy blip (sometimes surfacing as a totally
            // blank agent reply), so retry the call ONCE. Any other error type,
            // or a second consecutive empty-stream, propagates as before.
            let resp = {
                let mut attempt = 0u8;
                let mut rate_limit_attempt = 0u8;
                loop {
                    let req_try = req.clone();
                    let result = match &sink {
                        Some(s) if bubble_has_text => {
                            let (tx, fwd) = separated(s.clone());
                            let r = client.generate_stream(req_try, tx).await;
                            // Drain before anything else writes to `s` (the
                            // truncation markers below), so order holds.
                            let _ = fwd.await;
                            r
                        }
                        Some(s) => client.generate_stream(req_try, s.clone()).await,
                        None => client.generate(req_try).await,
                    };
                    match result {
                        Ok(r) => break r,
                        // Checked before the blip retry: asking the same
                        // question again only gets the same silence (turn 243).
                        Err(LlmError::InvalidResponse(ref msg))
                            if after_suggest_only
                                && !shown.is_empty()
                                && msg.contains("empty streamed response") =>
                        {
                            // Expected: the model deliberately ends its turn after
                            // suggest_replies (stop_reason=end_turn). The provider
                            // already warns with the stream details, so debug here.
                            tracing::debug!(
                                task_id,
                                iteration,
                                error = %msg,
                                "empty reply after suggest_replies; ending the turn on the shown text"
                            );
                            ledger.iterations = iteration;
                            ledger.stop = crate::turn_ledger::StopKind::EndTurn;
                            return Ok((
                                self.settle_turn(task_id, shown.join(SEGMENT_SEP), &ledger)
                                    .await,
                                None,
                            ));
                        }
                        Err(LlmError::InvalidResponse(ref msg))
                            if attempt == 0 && msg.contains("empty streamed response") =>
                        {
                            tracing::warn!(
                                task_id,
                                iteration,
                                after_suggest_only,
                                shown_chars = shown.iter().map(String::len).sum::<usize>(),
                                error = %msg,
                                "empty streamed response; retrying once"
                            );
                            attempt += 1;
                            continue;
                        }
                        // Transient 429: wait and retry, up to
                        // MAX_RATE_LIMIT_RETRIES times, so a momentary burst
                        // across parallel agents doesn't kill the turn outright.
                        // Prefer the server's own `retry-after` (clamped): it
                        // knows when the permit frees up and our doubling guess
                        // does not. Without it, fall back to the exponential
                        // schedule unchanged.
                        Err(LlmError::RateLimit(retry_after))
                            if rate_limit_attempt < MAX_RATE_LIMIT_RETRIES =>
                        {
                            rate_limit_attempt += 1;
                            let (delay, source) = match retry_after {
                                Some(d) => (d.min(crate::llm::RETRY_AFTER_MAX), "retry-after"),
                                None => (rate_limit_backoff_delay(rate_limit_attempt), "backoff"),
                            };
                            tracing::warn!(
                                attempt = rate_limit_attempt,
                                delay_secs = delay.as_secs(),
                                source,
                                "llm rate limited (429); backing off and retrying"
                            );
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        // Text already on the user's screen settles the turn
                        // as a truncation instead of a failure. A failed turn
                        // is not remembered and does not thread the next one,
                        // so the user would be answering a reply the agent
                        // has no record of (turn 243, "照 A 修").
                        Err(e) if !shown.is_empty() => {
                            self.last_turn_truncated.store(true, Ordering::Relaxed);
                            tracing::warn!(
                                task_id,
                                error = %e,
                                kept_chars = shown.iter().map(String::len).sum::<usize>(),
                                "llm call failed after text was shown; kept it as the reply"
                            );
                            ledger.iterations = iteration;
                            ledger.stop = crate::turn_ledger::StopKind::LlmFailedAfterOutput {
                                error: e.to_string(),
                            };
                            let mut text = shown.join(SEGMENT_SEP);
                            text.push_str(crate::llm::LLM_FAILED_TRUNCATION_MARKER);
                            return Ok((self.settle_turn(task_id, text, &ledger).await, None));
                        }
                        Err(e) => {
                            return Err(task_error("llm_error", format!("{e}"), true));
                        }
                    }
                }
            };

            if streaming && !resp.text.is_empty() {
                shown.push(resp.text.clone());
                bubble_has_text = true;
            }

            self.cumulative_input_tokens
                .fetch_add(resp.input_tokens, std::sync::atomic::Ordering::Relaxed);
            self.last_input_tokens
                .store(resp.input_tokens, std::sync::atomic::Ordering::Relaxed);
            self.cumulative_output_tokens
                .fetch_add(resp.output_tokens, std::sync::atomic::Ordering::Relaxed);
            *self
                .last_model_ref
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(resp.model.clone());

            // Truncation guard: a turn that hit the output-token ceiling AND
            // carries tool_calls was cut off MID-tool_use — the tool_use
            // `input` JSON is incomplete, so executing it (or appending it to
            // history and re-looping) just replays a malformed call and trips
            // the doom-loop guard. The same ceiling can also be hit while the
            // model is still inside a thinking block, before any text or
            // tool_use ever started — text and tool_calls both empty. Either
            // way there's nothing usable to show or execute, so append a
            // user-role guidance message and continue so the model can recover
            // with a shorter, well-formed turn. This counts toward the
            // iteration budget; the iteration cap + doom-loop guard remain the
            // backstops against a model that truncates forever.
            if resp.stop_reason == StopReason::MaxTokens
                && (!resp.tool_calls.is_empty() || resp.text.is_empty())
            {
                if !resp.text.is_empty() {
                    history.push(RichMessage::Text {
                        role: "assistant".into(),
                        content: resp.text.clone(),
                    });
                }
                history.push(RichMessage::Text {
                    role: "user".into(),
                    content: "Your previous response reached the output token limit \
                              and was truncated mid tool-call. Produce a shorter \
                              response, or write large files in smaller pieces \
                              (append in multiple steps)."
                        .into(),
                });
                iteration += 1;
                continue;
            }

            // A max_tokens stop that reaches this point carries usable text
            // and no tool calls (both other combinations were handled above):
            // the FINAL answer itself was cut off mid-generation. Mark it
            // visibly — in the returned reply, the streamed output, and the
            // persisted history — instead of passing truncated text off as a
            // complete answer (#715).
            let mut resp = resp;
            if resp.truncated_by_max_tokens() {
                self.mark_max_tokens_truncation(task_id, &mut resp);
                if let Some(s) = &sink {
                    let _ = s
                        .send(crate::llm::StreamDelta {
                            text: crate::llm::MAX_TOKENS_TRUNCATION_MARKER.to_string(),
                            thinking: false,
                        })
                        .await;
                }
            }
            // Second of the two sites (#1287). This repo has already shipped a
            // bug where one of a pair of identical response-handling sites was
            // updated and the other was not, so both carry this and a count
            // assertion in the tests guards the pair.
            if resp.stop_reason == StopReason::Interrupted {
                self.mark_stream_interruption(task_id, &mut resp);
                if let Some(s) = &sink {
                    let _ = s
                        .send(crate::llm::StreamDelta {
                            text: crate::llm::STREAM_IDLE_TRUNCATION_MARKER.to_string(),
                            thinking: false,
                        })
                        .await;
                }
            }

            // A provider that reports ToolUse owes us the calls it made, so
            // both being empty is a contradiction: the model called a tool and
            // the call was lost inside the provider client. Falling through
            // would push an empty ToolUse into history and hand `resp.text`
            // back as the reply — shipping the model's narration ("The exact
            // output is ...") to the user as though the tool had run.
            // Fabricated results are worse than a failed turn, so stop here.
            // Not recoverable: the same request against the same client
            // reproduces it, so a retry only repeats the lie (#938).
            if resp.stop_reason == StopReason::ToolUse && resp.tool_calls.is_empty() {
                return Err(task_error(
                    "llm_error",
                    format!(
                        "model {} reported a tool call but the provider client returned \
                         none; refusing to pass the model's own narration off as a tool \
                         result",
                        resp.model
                    ),
                    false,
                ));
            }

            history.push(RichMessage::ToolUse {
                text: if resp.text.is_empty() {
                    None
                } else {
                    Some(resp.text.clone())
                },
                calls: resp.tool_calls.clone(),
            });

            if resp.tool_calls.is_empty() || resp.stop_reason == StopReason::EndTurn {
                // ── Issue #001: the turn-continuation seam ────────────────
                // The ONLY place a turn ends of its own accord, and therefore
                // the only place "已授權工作持續推進" can mean anything. Before
                // this existed the instruction lived purely in the prompt, so
                // a model that decided it was done was done — the runtime had
                // no mechanism to re-enter the loop, and the user's standing
                // authorisation read as a suggestion.
                //
                // A clean stop is the model's own `EndTurn`. Every other way
                // out of this loop (ceiling, doom-loop, deadline, stuck,
                // truncation) returns elsewhere with its own graceful exit and
                // never reaches here, which is what keeps A1 true.
                let clean_stop = resp.stop_reason == StopReason::EndTurn;
                match mur_common::hitl::should_continue(
                    self.autonomy,
                    clean_stop,
                    gate_blocked,
                    continuations_used,
                ) {
                    Ok(()) => {
                        continuations_used += 1;
                        // Already on screen, in the bubble the next call keeps
                        // streaming into; the reply must carry it or it is
                        // erased when the reply replaces the streamed text.
                        if !resp.text.is_empty() {
                            carried.push(resp.text.clone());
                        }
                        if !resp.text.is_empty() {
                            history.push(RichMessage::Text {
                                role: "assistant".into(),
                                content: resp.text.clone(),
                            });
                        }
                        // A user-role nudge, not a system one: it has to be
                        // the same kind of message the standing authorisation
                        // would have been, and it has to be visibly a request
                        // the model may decline by finishing again.
                        history.push(RichMessage::Text {
                            role: "user".into(),
                            content: CONTINUE_NUDGE.into(),
                        });
                        iteration += 1;
                        continue;
                    }
                    Err(veto) => {
                        tracing::debug!(
                            autonomy = ?self.autonomy,
                            ?veto,
                            continuations_used,
                            "turn handed back"
                        );
                    }
                }
                ledger.iterations = iteration;
                ledger.stop = match resp.stop_reason {
                    StopReason::MaxTokens => crate::turn_ledger::StopKind::MaxTokens,
                    // Not `EndTurn`: nothing ended the turn, the stream went
                    // quiet. The ledger is a durable audit record and this is
                    // the only place that distinction survives.
                    StopReason::Interrupted => crate::turn_ledger::StopKind::StreamInterrupted,
                    _ => crate::turn_ledger::StopKind::EndTurn,
                };
                let reply = if carried.is_empty() {
                    resp.text
                } else {
                    carried.push(resp.text);
                    carried.retain(|t| !t.is_empty());
                    carried.join(SEGMENT_SEP)
                };
                return Ok((self.settle_turn(task_id, reply, &ledger).await, None));
            }

            // P3: gate the whole response first — one notification, N decisions.
            let (mut decisions, mut step_ids) = self.gate_response(task_id, &resp.tool_calls).await;
            let mut results = Vec::new();
            // Wall time per call, pushed in lockstep with `results` so the two
            // cannot drift. This is the only place a tool's own latency is
            // observable: `ToolResultEntry` does not carry it, and the dozen
            // other places that build one are synthesised errors and refusals
            // with no execution to measure (issue #1197).
            let mut durations_ms: Vec<u64> = Vec::new();
            // Step ids per call that actually ran, positionally matched to
            // `results`, so the post-hook token count below can be sent to
            // the card the client already opened for that call. `None` for
            // a withdrawn call: no step was ever started for it.
            let mut ran_step_ids: Vec<Option<String>> = Vec::new();
            for call in &resp.tool_calls {
                let t0 = std::time::Instant::now();
                // Withdrawn this turn (spec §3.8): the tool left the list
                // after a refusal; a model that calls it anyway is told so
                // again without the gate or the tool running.
                if disabled.contains(&call.tool_name) {
                    withdrawn_calls += 1;
                    durations_ms.push(0);
                    ran_step_ids.push(None);
                    results.push(crate::llm::ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: format!(
                            "`{}` was withdrawn for the rest of this turn after an authorization refusal; do not call it again",
                            call.tool_name
                        ),
                        is_error: true,
                        status: crate::tools::ToolStatus::Denied {
                            detail: "withdrawn this turn".into(),
                            scope: crate::tools::DenialScope::Tool,
                        },
                        images: Vec::new(),
                    });
                    continue;
                }
                // Minted here when the gate did not already (Allow-lane
                // calls), so the id is known on this side of the call too.
                let step_id = step_ids
                    .remove(&call.call_id)
                    .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
                match self
                    .handle_tool_call(
                        task_id,
                        call,
                        decisions.remove(&call.call_id),
                        Some(step_id.clone()),
                    )
                    .await
                {
                    Ok(entry) => {
                        durations_ms.push(t0.elapsed().as_millis() as u64);
                        ran_step_ids.push(Some(step_id));
                        results.push(entry);
                    }
                    Err(e) => return Err(e),
                }
            }

            // Fold post-tool-use hook patches into the results before
            // fingerprinting / history so the model sees the rewritten text:
            // CompressHook offloads oversized output (size-gated) and B0 rule 8
            // redacts PII. Patches are content-deterministic, so the doom-loop
            // fingerprint below stays stable.
            self.apply_post_tool_use(&resp.tool_calls, &mut results, &durations_ms)
                .await;
            // What the model will actually read — counted AFTER the hooks, so
            // a compressed result reports the size of the note, not the blob.
            self.emit_step_tokens(task_id, &ran_step_ids, &results)
                .await;

            // Doom-loop detection (safety layer): fingerprint every tool call
            // by (tool, args, RESULT) and abort if any single fingerprint
            // repeats `LOOP_REPEAT_THRESHOLD` times within the rolling window.
            // Keying on the result is the crux: "same command + same output,
            // repeated" = stuck (abort); "same command, changing output" =
            // making progress (don't abort). Fingerprinting happens here,
            // AFTER execution, because the result is needed.
            let mut progress_calls: Vec<(String, u64)> = Vec::with_capacity(results.len());
            for (call, entry) in resp.tool_calls.iter().zip(results.iter()) {
                if withdraws(entry) {
                    disabled.insert(call.tool_name.clone());
                }
                let outcome =
                    crate::turn_ledger::classify(&entry.content, entry.is_error, &entry.status);
                // #001 A2: a refusal is a human owed an answer. Latch it so no
                // autonomy setting can nudge the turn past a closed gate.
                if matches!(outcome, crate::turn_ledger::Outcome::Denied(_)) {
                    gate_blocked = true;
                }
                let target = crate::turn_ledger::describe_target_with_result(
                    &call.tool_name,
                    &call.input,
                    &entry.content,
                );
                let excerpt =
                    if call.tool_name == "bash" && outcome == crate::turn_ledger::Outcome::Ok {
                        crate::turn_ledger::excerpt_for(&target, &entry.content)
                    } else {
                        None
                    };
                ledger.record(crate::turn_ledger::Action {
                    tool: call.tool_name.clone(),
                    target,
                    outcome,
                    excerpt,
                });
                let fp = (
                    call.tool_name.clone(),
                    fingerprint_args(&call.input),
                    fingerprint_str(&entry.content),
                );
                fingerprints.push_back(fp.clone());
                while fingerprints.len() > LOOP_WINDOW {
                    fingerprints.pop_front();
                }
                let repeats = fingerprints.iter().filter(|f| **f == fp).count();
                // D4: a yield that delivered new bytes is progress; one that
                // delivered nothing repeats the previous fingerprint exactly.
                let mut args_fp = fingerprint_args(&call.input);
                if let crate::tools::ToolStatus::Running { bytes_seen, .. } = &entry.status {
                    args_fp ^= fingerprint_str(&format!("bytes_seen:{bytes_seen}"));
                }
                progress_calls.push((call.tool_name.clone(), args_fp));
                if repeats >= LOOP_REPEAT_THRESHOLD {
                    // Append the results gathered this turn before exiting so
                    // the dangling tool_use is closed; graceful_exit also
                    // sanitizes, but keeping history consistent is cheap.
                    history.push(RichMessage::ToolResults { results });
                    let msg = self
                        .graceful_exit(
                            task_id,
                            client,
                            &history,
                            LoopStop::LoopDetected,
                            &ledger,
                            iteration,
                            &progress,
                        )
                        .await;
                    return Ok((
                        msg,
                        Some(LoopExit {
                            reason: LoopStop::LoopDetected,
                            iterations: iteration,
                        }),
                    ));
                }
            }

            // Nothing this iteration ran: every call went to a tool that is
            // gone for the turn. Settle now rather than let the model keep
            // paying for refusals it cannot act on. Placed after the ledger
            // loop above so the wasted calls are still on the audit record.
            if withdrawn_calls >= WITHDRAWN_CALL_LIMIT {
                history.push(RichMessage::ToolResults { results });
                let msg = self
                    .graceful_exit(
                        task_id,
                        client,
                        &history,
                        LoopStop::ToolWithdrawn,
                        &ledger,
                        iteration,
                        &progress,
                    )
                    .await;
                return Ok((
                    msg,
                    Some(LoopExit {
                        reason: LoopStop::ToolWithdrawn,
                        iterations: iteration,
                    }),
                ));
            }

            history.push(RichMessage::ToolResults { results });

            // Note: `suggest_replies` does NOT hard-end the turn. Whether to stop
            // and wait for the user's pick, or keep going, is the model's call —
            // it ends the turn by emitting `stop_reason: end_turn` after offering
            // the options (soft-guided by the tool description) when it needs the
            // answer, and continues when it already knows the next step. Forcing
            // an end here would rob the model of that judgement.

            // Mid-turn steering: pick up any user interjection sent via turn/steer
            // since the last LLM call and append it before the next iteration.
            // Race-free: history is mutated only here; try_recv never blocks.
            if let Some(rx) = steer_rx.as_mut() {
                while let Ok(msg) = rx.try_recv() {
                    history.push(RichMessage::Text {
                        role: "user".into(),
                        content: format!("(steering) {msg}"),
                    });
                }
            }
            after_suggest_only = !resp.tool_calls.is_empty()
                && resp
                    .tool_calls
                    .iter()
                    .all(|c| crate::tools::suggest::suggest_replies_allowed(&c.tool_name));
            if !after_suggest_only {
                // A real tool drew a card; the text above it is frozen there.
                carried.clear();
                bubble_has_text = false;
            } else if !resp.text.is_empty() {
                carried.push(resp.text.clone());
            }
            progress.observe(&progress_calls, std::time::Instant::now());
            iteration += 1;
        }

        let msg = self
            .graceful_exit(
                task_id,
                client,
                &history,
                LoopStop::IterationCeiling,
                &ledger,
                iteration,
                &progress,
            )
            .await;
        Ok((
            msg,
            Some(LoopExit {
                reason: LoopStop::IterationCeiling,
                iterations: iteration,
            }),
        ))
    }
}
