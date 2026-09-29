use super::*;

impl TaskRunner {
    // ponytail: one private method, one extra threaded id — an args struct would
    // be ceremony for no caller benefit.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_llm(
        &self,
        task_id: &str,
        client: &dyn LlmClient,
        input: &Message,
        context_task_id: Option<&str>,
        active_fleet: Option<&str>,
        active_team: Option<&str>,
        output_artifact_path: Option<&std::path::Path>,
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
        intent: RequestIntent,
    ) -> Result<Message, TaskError> {
        let prompt = text_of(input);

        let (mut system, fired) =
            self.assemble_system_prompt(Some(task_id), &prompt, active_fleet, active_team);
        if let Some(path) = output_artifact_path {
            let rule = ARTIFACT_RULE.replace("{path}", &path.to_string_lossy());
            system.push_str(&rule);
        }

        // Apply hook chain on_prompt_submit if wired.
        let system = if let (Some(chain), Some(ctx), Some(cancel)) =
            (&self.hook_chain, &self.hook_ctx, &self.hook_cancel)
        {
            if cancel.is_cancelled() {
                return Err(task_error(
                    "cancelled",
                    "cancelled before prompt submit".to_string(),
                    true,
                ));
            }
            let mut turn_ctx = ctx.clone();
            turn_ctx.turn_id = self.turn_counter.load(Ordering::Relaxed);
            let view = PromptView {
                system: Some(system),
                messages: vec![serde_json::json!({"role": input.role, "content": prompt})],
            };
            let patch = chain.on_prompt_submit(&turn_ctx, &view, cancel).await;
            {
                let mut s = view.system.unwrap_or_default();
                if let Some(prefix) = patch.set_system_prefix {
                    s = format!("{prefix}\n{s}");
                }
                if let Some(suffix) = patch.set_system_suffix {
                    s = format!("{s}\n{suffix}");
                }
                s
            }
        } else {
            system
        };

        // Seed with prior conversation threaded via `context.task_id` so the
        // model has multi-turn memory (was: system + this message only).
        let (pinned, prior) = self.pinned_and_prior(task_id, context_task_id);
        let messages = seed_history(system, pinned, prior, input);
        let req = LlmRequest {
            messages,
            temperature: None,
            max_tokens: None,
            tools: vec![],
            effort: self.effort(),
            intent,
            task_id: Some(task_id.to_string()),
            ..Default::default()
        };
        let start = std::time::Instant::now();
        let llm_result = match &sink {
            Some(s) => client.generate_stream(req, s.clone()).await,
            None => client.generate(req).await,
        };
        match llm_result {
            Ok(mut resp) => {
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
                // The stream stopped sending (#1287). Same three destinations as
                // the max_tokens marker above — returned reply, streamed output,
                // persisted history — because a UI that only ever saw the
                // deltas would show a truncated answer as a complete one.
                if resp.stop_reason == crate::llm::StopReason::Interrupted {
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
                let latency_ms = start.elapsed().as_millis() as u64;
                *self
                    .last_model_ref
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = Some(resp.model.clone());
                let _prev = self
                    .cumulative_input_tokens
                    .fetch_add(resp.input_tokens, Ordering::Relaxed);
                self.last_input_tokens
                    .store(resp.input_tokens, Ordering::Relaxed);
                self.cumulative_output_tokens
                    .fetch_add(resp.output_tokens, Ordering::Relaxed);
                if let Some(tx) = &self.telemetry {
                    // Emit per-skill SkillExecuted events (M5a). Outcome is
                    // NotEvaluated because the M2 wiring doesn't track
                    // per-skill success/failure; the aggregator infers from
                    // LLM-call context.
                    let skill_events: Vec<Event> = fired
                        .iter()
                        .filter_map(|name| {
                            let loaded = self.skills.as_ref().and_then(|s| {
                                s.snapshot()
                                    .loaded
                                    .iter()
                                    .find(|l| &l.name == name)
                                    .cloned()
                            })?;
                            Some(Event::SkillExecuted {
                                trace_id: task_id.to_string(),
                                task_id: task_id.to_string(),
                                skill_name: name.clone(),
                                skill_version: loaded.manifest.version.clone(),
                                manifest_digest: loaded.content_hash.clone(),
                                outcome: SkillOutcome::NotEvaluated,
                                duration_ms: latency_ms,
                            })
                        })
                        .collect();
                    let _ = tx
                        .send(Event::LlmCall {
                            trace_id: task_id.to_string(),
                            task_id: task_id.to_string(),
                            model: resp.model.clone(),
                            input_tokens: resp.input_tokens,
                            output_tokens: resp.output_tokens,
                            cache_creation_input_tokens: resp.cache_creation_input_tokens,
                            cache_read_input_tokens: resp.cache_read_input_tokens,
                            latency_ms,
                            cost_usd: 0.0,
                            provider: "ollama".into(),
                            fired_skills: fired,
                        })
                        .await;
                    for ev in skill_events {
                        let _ = tx.send(ev).await;
                    }
                }
                Ok(Message {
                    role: "agent".into(),
                    parts: vec![MessagePart::Text { text: resp.text }],
                })
            }
            Err(e) => Err(task_error("llm_error", format!("{e}"), true)),
        }
    }

    pub(super) fn tools_for_loop(&self) -> &[Arc<dyn crate::tools::ToolExecutor>] {
        &self.tools
    }

    /// Handle a final answer the provider cut off at the max_tokens ceiling:
    /// warn loudly, flag the turn for `Task.usage` (`"truncated": true`), and
    /// append the visible truncation marker so every downstream consumer —
    /// user, delegating agent, channel history — can see the cut instead of a
    /// silent mid-word seam (#715). The effective ceiling is the model's
    /// `models.yaml` `max_tokens:` when set, else the provider default
    /// (requests leave `max_tokens` unset), so the warning reports the
    /// actual `output_tokens`, which equals the cap at truncation.
    pub(super) fn mark_max_tokens_truncation(
        &self,
        task_id: &str,
        resp: &mut crate::llm::LlmResponse,
    ) {
        self.last_turn_truncated.store(true, Ordering::Relaxed);
        tracing::warn!(
            agent = self
                .hook_ctx
                .as_ref()
                .map(|c| c.agent_name.as_str())
                .unwrap_or("<unknown>"),
            task_id,
            model = %resp.model,
            output_tokens = resp.output_tokens,
            "llm generation hit the max_tokens ceiling; reply is truncated (visible marker appended)"
        );
        resp.text.push_str(crate::llm::MAX_TOKENS_TRUNCATION_MARKER);
    }

    /// Mark a reply whose stream stopped sending (#1287).
    ///
    /// Reuses `last_turn_truncated`, and therefore the existing `truncated`
    /// flag on the usage JSON, rather than adding a wire field: an interrupted
    /// reply is truncated for every purpose the `max_tokens` marker exists for.
    /// The ledger is where the two are told apart
    /// (`StopKind::StreamInterrupted`).
    ///
    /// `output_tokens` is deliberately not logged here the way the max_tokens
    /// helper logs it: providers send usage in the final frame, which by
    /// definition never arrived, so the value is 0 and printing it would read
    /// as "this reply cost nothing".
    pub(super) fn mark_stream_interruption(
        &self,
        task_id: &str,
        resp: &mut crate::llm::LlmResponse,
    ) {
        self.last_turn_truncated.store(true, Ordering::Relaxed);
        tracing::warn!(
            agent = self
                .hook_ctx
                .as_ref()
                .map(|c| c.agent_name.as_str())
                .unwrap_or("<unknown>"),
            task_id,
            model = %resp.model,
            kept_chars = resp.text.len(),
            "llm stream stopped sending; partial reply kept (visible marker appended)"
        );
        resp.text
            .push_str(crate::llm::STREAM_IDLE_TRUNCATION_MARKER);
    }

    /// Resolve every `Ask` call of one response before any executes. Calls whose
    /// policy is not `Ask`, or whose tool is unknown, get no entry and take the
    /// existing Allow/Deny/unknown-tool arms. Fail-closed exactly as before: no
    /// sink → deny; a caller that declared `can_approve: false` → deny without
    /// asking. Returns `call_id → decision` and `call_id → step_id` (the id the
    /// notification carried, so the client can mark the card that ran it).
    pub(super) async fn gate_response(
        &self,
        task_id: &str,
        calls: &[crate::llm::ToolCallResult],
    ) -> (
        HashMap<String, crate::hitl::HitlDecision>,
        HashMap<String, String>,
    ) {
        self.guarded().gate_response(task_id, calls).await
    }

    pub(super) async fn handle_tool_call(
        &self,
        task_id: &str,
        call: &crate::llm::ToolCallResult,
        decision: Option<crate::hitl::HitlDecision>,
        step_id: Option<String>,
    ) -> Result<crate::llm::ToolResultEntry, TaskError> {
        self.guarded()
            .handle_tool_call(task_id, call, decision, step_id)
            .await
    }

    /// A `GuardedToolCall` over this runner's current configuration.
    ///
    /// Built per call rather than held as a field: `tools` and `tools_policy`
    /// are replaced by the `with_*` builders after construction, so a cached
    /// copy would serve a stale policy — the one kind of staleness that
    /// silently widens what a tool may do.
    pub(crate) fn guarded(&self) -> crate::tools::guarded::GuardedToolCall {
        crate::tools::guarded::GuardedToolCall {
            tools: self.tools.clone(),
            tools_policy: self.tools_policy.clone(),
            secrets: self.secrets.clone(),
            notifier: self.notifier.clone(),
            client_notifiers: self.client_notifiers.clone(),
            approval_sinks: self.approval_sinks.clone(),
            agent_name: self.agent_name.clone(),
            decision_store: self.decision_store.clone(),
            hitl_timeout_secs: self.hitl_timeout_secs,
            pending_approvals: self.pending_approvals.clone(),
            shim_trust: Some(self.shim_trust.clone()),
            sandbox_enforcing: self.sandbox_enforcing,
        }
    }

    /// Fold the hook chain's `post_tool_use` patch into each tool result,
    /// rewriting `content` when a hook returns `replace_output` — e.g.
    /// `CompressHook` offloading an oversized output (size-gated by
    /// `compress.yaml` `auto.min_tokens`) or B0 rule 8 PII redaction.
    /// Mutates in place; no-op when no chain is wired or every hook returns
    /// `None`. Closes the M7.6 gap where the patch was computed but discarded.
    ///
    /// `entry.images` is deliberately untouched: a hook patches the model-facing
    /// TEXT, and the compress path in particular offloads text by size — an
    /// image is not counted in that budget and swapping its bytes for a digest
    /// would leave the model looking at nothing. A hook that must suppress an
    /// image should deny the tool call, not blank the result.
    /// `durations_ms` is per-call wall time, positionally matched to `calls`.
    /// A short slice yields 0 for the tail rather than skipping those calls:
    /// a hook that does not run can drop a compress offload or a PII
    /// redaction, which is a worse failure than an unmeasured call.
    pub(super) async fn apply_post_tool_use(
        &self,
        calls: &[crate::llm::ToolCallResult],
        results: &mut [crate::llm::ToolResultEntry],
        durations_ms: &[u64],
    ) {
        let (Some(chain), Some(ctx), Some(cancel)) =
            (&self.hook_chain, &self.hook_ctx, &self.hook_cancel)
        else {
            return;
        };
        let mut turn_ctx = ctx.clone();
        turn_ctx.turn_id = self.turn_counter.load(Ordering::Relaxed);
        for (i, (call, entry)) in calls.iter().zip(results.iter_mut()).enumerate() {
            let tc = ToolCall {
                tool_name: call.tool_name.clone(),
                mcp_server: None,
                call_id: call.call_id.clone(),
                input: call.input.clone(),
            };
            let tr = ToolResult {
                call_id: entry.call_id.clone(),
                ok: !entry.is_error,
                output: serde_json::Value::String(entry.content.clone()),
                duration_ms: durations_ms.get(i).copied().unwrap_or(0),
            };
            if let Some(v) = chain
                .post_tool_use(&turn_ctx, &tc, &tr, cancel)
                .await
                .replace_output
            {
                entry.content = match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
            }
        }
    }

    /// One line into the live transcript of the connection that holds this
    /// turn, as a `message/delta` text frame — the frame murmur already
    /// renders, so no new frame type and no client change. Attended turns
    /// only; nobody is reading an unattended sink.
    pub(super) async fn emit_live_warning(&self, task_id: &str, text: &str) {
        let entry = self.client_notifiers.lock().await.get(task_id).cloned();
        if let Some((tx, _)) = entry {
            let _ = tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "message/delta",
                    "params": { "task_id": task_id, "text": format!("\n{text}\n"), "thinking": false },
                }))
                .await;
        }
    }

    /// One final, tools-DISABLED LLM turn asking the model to summarize what it
    /// completed, the current build/test state, and the remaining steps. If that
    /// call fails, fall back to the last assistant text already in `history` so
    /// accumulated work is never lost.
    pub(super) async fn graceful_exit(
        &self,
        client: &dyn crate::llm::LlmClient,
        history: &[crate::llm::RichMessage],
        reason: LoopStop,
        ledger: &crate::turn_ledger::TurnLedger,
        iterations: u32,
        progress: &crate::bounds::Progress,
    ) -> Message {
        use crate::llm::{LlmRequest, RichMessage};

        let why = match reason {
            LoopStop::Deadline => "the deadline for this task has passed".to_string(),
            LoopStop::Stuck => format!(
                "no progress was made — the last tool calls were: {}",
                progress.last_calls()
            ),
            LoopStop::LoopDetected => {
                "the last tool call repeated with identical arguments and output".to_string()
            }
            LoopStop::IterationCeiling => {
                format!("the {ITERATION_CEILING}-iteration safety ceiling was hit")
            }
            LoopStop::ToolWithdrawn => {
                "a tool you kept calling was withdrawn earlier this turn after a refusal; \
                 it will not come back before the turn ends"
                    .to_string()
            }
        };
        let nudge = format!(
            "Stop calling tools: {why}. Summarize what you completed, the current \
             build/test state, and the remaining steps so work can resume later."
        );
        // When a budget aborts MID-iteration (e.g. the doom-loop guard fires
        // after the model emitted a tool_use but before its tool_result was
        // appended), `history` ends with a tool_use that has no matching
        // tool_result. The Anthropic API rejects such a request with HTTP 400.
        // Close every dangling tool_use with a synthetic tool_result so the
        // summary request is well-formed.
        let mut messages = sanitize_dangling_tool_uses(history, reason);
        messages.push(RichMessage::Text {
            role: "user".into(),
            content: nudge,
        });
        let req = LlmRequest {
            messages,
            temperature: None,
            max_tokens: None,
            // Mechanical: recap what happened, no tools, no decisions — and it
            // fires exactly when a budget or loop guard already tripped, so
            // inheriting the agent's `xhigh` for a post-mortem is the wrong
            // direction. Pinned low regardless of the profile.
            effort: Some(mur_common::llm::Effort::Low),
            tools: vec![], // tools disabled: force a textual summary
            ..Default::default()
        };
        let text = match client.generate(req).await {
            Ok(resp) => {
                self.cumulative_input_tokens
                    .fetch_add(resp.input_tokens, std::sync::atomic::Ordering::Relaxed);
                self.last_input_tokens
                    .store(resp.input_tokens, std::sync::atomic::Ordering::Relaxed);
                self.cumulative_output_tokens
                    .fetch_add(resp.output_tokens, std::sync::atomic::Ordering::Relaxed);
                resp.text
            }
            Err(_) => {
                // Never lose work: return the last assistant text from history.
                last_assistant_text(history)
                    .unwrap_or_else(|| format!("Stopped: {}.", reason.as_str()))
            }
        };
        // #595: mark output that ended early so partial execution is visible
        // instead of silently reported as clean. This used to be a string
        // appended after whatever the model had just claimed, which let the two
        // contradict each other in the same reply; it is now a row in the
        // settlement, next to what did and did not actually run.
        let ledger = crate::turn_ledger::TurnLedger {
            stop: match reason {
                LoopStop::IterationCeiling => crate::turn_ledger::StopKind::MaxIterations,
                LoopStop::LoopDetected => crate::turn_ledger::StopKind::LoopDetected,
                LoopStop::Deadline => crate::turn_ledger::StopKind::Deadline,
                LoopStop::Stuck => crate::turn_ledger::StopKind::Stuck {
                    last_calls: progress.last_calls(),
                },
                LoopStop::ToolWithdrawn => crate::turn_ledger::StopKind::ToolWithdrawn,
            },
            // Use the live counter passed in, not `ledger.iterations`: on the
            // early-exit paths the caller's ledger was never updated with the
            // current count, so trusting the field here would render "0
            // iterations" even though the counter itself is correct.
            iterations,
            ..ledger.clone()
        };
        settle(text, &ledger)
    }
}
