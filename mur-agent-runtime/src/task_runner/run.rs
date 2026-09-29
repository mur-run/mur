use super::*;

impl TaskRunner {
    pub async fn run_sync(&self, spec: TaskSpec) -> TaskOutcome {
        self.run_sync_inner(spec, None, None).await
    }

    /// Like `run_sync`, but forwards each LLM token delta to `sink` as it is
    /// generated (used by message/send streaming). The final task is returned
    /// as usual once generation completes.
    pub async fn run_sync_streaming(
        &self,
        spec: TaskSpec,
        sink: tokio::sync::mpsc::Sender<crate::llm::StreamDelta>,
        steer_rx: Option<tokio::sync::mpsc::Receiver<String>>,
    ) -> TaskOutcome {
        self.run_sync_inner(spec, Some(sink), steer_rx).await
    }

    pub(super) async fn run_sync_inner(
        &self,
        spec: TaskSpec,
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
        steer_rx: Option<tokio::sync::mpsc::Receiver<String>>,
    ) -> TaskOutcome {
        // Reject new turns immediately when the runtime is draining for restart.
        // This is a transient failure — callers should retry after the agent
        // comes back up. We do NOT register the task in the registry, so
        // `await_idle` will not be blocked by this rejection.
        if self.draining.load(Ordering::SeqCst) {
            let id = spec
                .task_id
                .clone()
                .unwrap_or_else(|| format!("task-{}", Uuid::now_v7()));
            let now = chrono::Utc::now().to_rfc3339();
            return TaskOutcome::Failed(Task {
                id,
                state: TaskState::Failed,
                messages: vec![spec.input],
                created_at: now.clone(),
                completed_at: Some(now),
                error: Some(task_error(
                    "draining",
                    "agent is draining for restart; retry shortly".into(),
                    true,
                )),
                usage: None,
                artifacts: None,
            });
        }
        // Record real inbound activity so idle triggers measure genuine
        // quiescence. Previously only `start_async` (a non-production path)
        // bumped this, leaving `last_activity_at` permanently 0 and causing
        // every idle trigger to fire on its first tick.
        self.last_activity_at
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
        // Use the caller-supplied id when present so the client can cancel by an
        // id it already holds; otherwise generate one (back-compatible).
        let id = spec
            .task_id
            .clone()
            .unwrap_or_else(|| format!("task-{}", Uuid::now_v7()));
        self.set_state(&id, TaskState::Working);

        // Register a cancel signal so `tasks/cancel{id}` can abort this in-flight
        // generation. Mirrors `start_async`, but for the inline return-value path.
        let (tx_cancel, mut rx_cancel) = oneshot::channel::<()>();
        self.cancel_signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx_cancel);

        // Snapshot the runner-lifetime token counters so we can report THIS
        // turn's real input+output usage as a delta in `Task.usage`. Same
        // snapshot-delta approach (and concurrency caveat — concurrent turns on
        // one runner can inflate a delta) the agentic loop's own token budget
        // already uses; fine for the serial-delegate fleet path.
        let tok_in0 = self.cumulative_input_tokens.load(Ordering::Relaxed);
        let tok_out0 = self.cumulative_output_tokens.load(Ordering::Relaxed);
        // Clear the previous turn's model_ref so a stub/misconfigured backend
        // (which never calls a real model) reports none rather than stale data.
        *self
            .last_model_ref
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        // Same for the truncation flag: it describes THIS turn only.
        self.last_turn_truncated.store(false, Ordering::Relaxed);

        let output_artifact_path = spec.output_artifact_path.clone();
        self.adopt_cwd(&id, spec.context_task_id.as_deref(), spec.cwd.as_deref());
        let generation = async {
            match &self.backend {
                RunnerBackend::StubEcho => Ok((echo_response(&spec.input), None)),
                RunnerBackend::StubSlow => {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Ok((echo_response(&spec.input), None))
                }
                RunnerBackend::Misconfigured(message) => Ok((text_response(message), None)),
                RunnerBackend::CliSpawn(backend) => {
                    let Some(socket) = self.socket_path.clone() else {
                        // Fail closed and say why: without the socket the
                        // shim cannot dial back, so the CLI would run with
                        // no MUR tools at all — a working-looking turn with
                        // none of the guarantees.
                        return Ok((
                            text_response(
                                "cli-spawn backend has no agent socket configured; refusing to spawn",
                            ),
                            None,
                        ));
                    };
                    let shim = std::env::current_exe()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "mur-agent-runtime".to_string());
                    // Revoked when `ticket` drops at the end of this arm —
                    // success, error and cancellation alike.
                    let ticket = self.shim_trust.issue(&id);
                    let reply = crate::cli_spawn::run_turn(crate::cli_spawn::SpawnRequest {
                        backend,
                        ticket: Some(&ticket),
                        mur_home: &mur_common::trust::mur_home(),
                        shim_bin: &shim,
                        socket: &socket,
                        task_id: &id,
                        prompt: &text_of(&spec.input),
                        // The same sink the gateway track writes to. A spawned
                        // turn is watchable for the same reason and by the same
                        // frame; murmur needs no knowledge of which track ran.
                        deltas: sink,
                    })
                    .await
                    // `recoverable: false` — a spawn that failed to start,
                    // or a CLI that exited non-zero, does not become healthy
                    // by running the same turn again.
                    .map_err(|e| task_error("cli_spawn_failed", format!("{e}"), false))?;
                    Ok((text_response(&reply), None))
                }
                RunnerBackend::Llm(client) => {
                    if self.pending_approvals.is_some() {
                        let mut system = self
                            .prepare_system_prompt(
                                &id,
                                &spec.input,
                                spec.active_fleet.as_deref(),
                                spec.active_team.as_deref(),
                            )
                            .await
                            .unwrap_or_default();
                        // Append the artifact-output rule when the caller set an
                        // output path — tells the agent to write the file and
                        // return ONLY the path, never the content (avoiding
                        // re-typing corruption, #715 Part B).
                        if let Some(ref path) = output_artifact_path {
                            let rule = ARTIFACT_RULE.replace("{path}", &path.to_string_lossy());
                            system.push_str(&rule);
                        }
                        // Resolved before the first LLM call so an unparsable
                        // limits: value fails the task naming the key (§4).
                        let bounds = self.bounds_for(&spec)?;
                        self.run_agentic_loop(
                            &id,
                            client.as_ref(),
                            system,
                            &spec.input,
                            spec.context_task_id.as_deref(),
                            sink,
                            steer_rx,
                            spec.intent,
                            bounds,
                        )
                        .await
                    } else {
                        self.run_llm(
                            &id,
                            client.as_ref(),
                            &spec.input,
                            spec.context_task_id.as_deref(),
                            spec.active_fleet.as_deref(),
                            spec.active_team.as_deref(),
                            output_artifact_path.as_deref(),
                            sink,
                            spec.intent,
                        )
                        .await
                        .map(|m| (m, None))
                    }
                }
            }
        };

        // Race generation against the cancel signal. On cancel, the generation
        // future is dropped (Rust async cancellation aborts the in-flight LLM
        // call) and we return a Cancelled task so message/send terminates the
        // stream cleanly.
        let result: Option<Result<(Message, Option<LoopExit>), TaskError>> = tokio::select! {
            r = generation => Some(r),
            _ = &mut rx_cancel => None,
        };

        // Always remove the cancel entry (success, failure, or cancel) to avoid
        // leaking senders in `cancel_signals`.
        self.cancel_signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);

        // Real token usage for THIS turn (delta of the runner-lifetime counters
        // from the pre-generation snapshot). Reported on EVERY outcome — a turn
        // that burned tokens then failed or was cancelled must still be
        // accounted, or the fleet budget guard would under-count (the dangerous
        // direction for a spend cap).
        let token_usage = || {
            let input_tokens = self
                .cumulative_input_tokens
                .load(Ordering::Relaxed)
                .saturating_sub(tok_in0);
            let output_tokens = self
                .cumulative_output_tokens
                .load(Ordering::Relaxed)
                .saturating_sub(tok_out0);
            // `model_ref` = the winning model of the most recent successful LLM
            // call this turn (None for stub/misconfigured backends). `route_reason`
            // is a best-effort label derived from `spec.intent` alone — NOT the
            // real per-call `FallbackLlmClient::selection_reason` outcome (that
            // lives behind the `LlmClient` trait object and isn't threaded back
            // through `LlmResponse`; wiring it through would mean growing the
            // trait's return type across every provider, a bigger refactor than
            // this field is worth). "smart-background" here means "this request
            // was tagged Background and therefore eligible for Smart routing",
            // not "Smart definitely picked the cheap model".
            let model_ref = self
                .last_model_ref
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let route_reason = match spec.intent {
                RequestIntent::Interactive => "interactive",
                RequestIntent::Background(_) => "smart-background",
            };
            let mut usage = serde_json::json!({
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "context_tokens": self.last_input_tokens.load(Ordering::Relaxed),
                "model_ref": model_ref,
                "route_reason": route_reason,
            });
            // Additive: present (true) only when the final generation was cut
            // off at the max_tokens ceiling, so existing consumers of the
            // usage JSON are unaffected (#715).
            if self.last_turn_truncated.load(Ordering::Relaxed) {
                usage["truncated"] = true.into();
            }
            usage
        };

        let now = chrono::Utc::now().to_rfc3339();
        let result = match result {
            None => {
                self.set_state(&id, TaskState::Cancelled);
                return TaskOutcome::Cancelled(Task {
                    id,
                    state: TaskState::Cancelled,
                    messages: vec![spec.input],
                    created_at: now.clone(),
                    completed_at: Some(now),
                    error: None,
                    artifacts: None,
                    usage: Some(token_usage()),
                });
            }
            Some(r) => r,
        };
        match result {
            Ok((reply, stop)) => {
                self.set_state(&id, TaskState::Completed);
                // Persist this turn into multi-turn chat memory keyed by `id`, so
                // the next send (context.task_id == id) recalls it. Only on
                // success — a failed/cancelled turn must not leave a dangling
                // user message with no assistant reply.
                self.remember_turn(&id, spec.context_task_id.as_deref(), &spec.input, &reply);
                // Artifact detection (#715 Part B): when the caller set an
                // output_artifact_path and the agent wrote to it, replace the
                // reply with a short path reference and populate Task.artifacts
                // so callers read the file byte-by-byte instead of re-typing
                // content through another LLM.
                let artifacts = output_artifact_path.and_then(|p| detect_artifact(&p));
                let reply = if let Some(ref arts) = artifacts {
                    Message {
                        role: reply.role.clone(),
                        parts: vec![MessagePart::Text {
                            text: format!(
                                "[Artifact: {} ({} bytes)]",
                                arts[0].path, arts[0].size_bytes
                            ),
                        }],
                    }
                } else {
                    reply
                };
                // Report this turn's real token usage (delta of the lifetime
                // counters) so callers — notably the fleet budget guard — can
                // account actual spend instead of a projection. When a budget
                // forced an early, graceful exit, also surface the reason +
                // iteration count so callers can tell a truncated completion
                // from a natural one (the task still reports Completed — work is
                // preserved, not failed).
                let mut usage_obj = token_usage();
                if let Some(exit) = stop {
                    usage_obj["stop_reason"] = exit.reason.as_str().into();
                    usage_obj["iterations"] = exit.iterations.into();
                }
                let mut usage = Some(usage_obj);
                // Attach artifacts to the task result so callers find them in
                // the same JSON as the reply (additive: absent on non-artifact
                // turns, so existing consumers are unaffected).
                if let Some(ref arts) = artifacts {
                    let u = usage.get_or_insert_with(|| serde_json::json!({}));
                    u["artifacts"] = serde_json::to_value(arts).unwrap_or_default();
                }
                TaskOutcome::Completed(Task {
                    id,
                    state: TaskState::Completed,
                    messages: vec![spec.input, reply],
                    created_at: now.clone(),
                    completed_at: Some(now),
                    error: None,
                    usage,
                    artifacts,
                })
            }
            Err(err) => {
                // A provider/runtime failure must surface as Failed with a
                // populated `error` — not a Completed task whose reply body
                // happens to contain "llm error:". Callers (message/send) and
                // the scheduler's `Failed` branch rely on this distinction.
                tracing::warn!(
                    task_id = %id,
                    code = %err.code,
                    error = %err.message,
                    "task failed"
                );
                self.set_state(&id, TaskState::Failed);
                TaskOutcome::Failed(Task {
                    id,
                    state: TaskState::Failed,
                    messages: vec![spec.input],
                    created_at: now.clone(),
                    completed_at: Some(now),
                    error: Some(err),
                    // Account tokens already burned before the failure (a long
                    artifacts: None,
                    // agentic turn can error after real spend) — never drop it.
                    usage: Some(token_usage()),
                })
            }
        }
    }

    pub fn start_async(&self, spec: TaskSpec) -> AsyncTaskHandle {
        // Drain guard: reject new tasks when the runtime is draining for restart.
        // Must run BEFORE any set_state(_, Working) so await_idle is never blocked
        // by a phantom Working entry.
        if self.draining.load(Ordering::SeqCst) {
            tracing::debug!(
                "start_async called while draining — returning transient failure without registering a Working entry"
            );
            let id = format!("task-{}", Uuid::now_v7());
            let now = chrono::Utc::now().to_rfc3339();
            let (tx_done, rx_done) = oneshot::channel::<TaskOutcome>();
            let _ = tx_done.send(TaskOutcome::Failed(Task {
                id: id.clone(),
                state: TaskState::Failed,
                messages: vec![spec.input],
                created_at: now.clone(),
                completed_at: Some(now),
                error: Some(task_error(
                    "draining",
                    "agent is draining for restart; retry shortly".into(),
                    true,
                )),
                usage: None,
                artifacts: None,
            }));
            return AsyncTaskHandle { id, done: rx_done };
        }
        self.last_activity_at
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
        let id = format!("task-{}", Uuid::now_v7());
        let (tx_done, rx_done) = oneshot::channel::<TaskOutcome>();
        let (tx_cancel, mut rx_cancel) = oneshot::channel::<()>();
        self.cancel_signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), tx_cancel);
        self.set_state(&id, TaskState::Working);
        let id_clone = id.clone();
        let registry = self.registry.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    let reply = echo_response(&spec.input);
                    registry.lock().unwrap_or_else(|e| e.into_inner()).insert(id_clone.clone(), TaskState::Completed);
                    let _ = tx_done.send(TaskOutcome::Completed(Task {
                        id: id_clone.clone(),
                        state: TaskState::Completed,
                        messages: vec![spec.input, reply],
                artifacts: None,
                        created_at: chrono::Utc::now().to_rfc3339(),
                        completed_at: Some(chrono::Utc::now().to_rfc3339()),
                        error: None,
                        usage: None,
                    }));
                }
                _ = &mut rx_cancel => {
                    registry.lock().unwrap_or_else(|e| e.into_inner()).insert(id_clone.clone(), TaskState::Cancelled);
                    let _ = tx_done.send(TaskOutcome::Cancelled(Task {
                        id: id_clone.clone(),
                        state: TaskState::Cancelled,
                artifacts: None,
                        messages: vec![spec.input],
                        created_at: chrono::Utc::now().to_rfc3339(),
                        completed_at: Some(chrono::Utc::now().to_rfc3339()),
                        error: None,
                        usage: None,
                    }));
                }
            }
        });
        AsyncTaskHandle { id, done: rx_done }
    }

    pub async fn cancel(&self, task_id: &str) -> Result<(), String> {
        let tx = self
            .cancel_signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
        // Whether or not the generation is still cancellable, the task's
        // jobs are (D3): a cancel means "stop everything this task started".
        self.kill_jobs_of(task_id).await;
        match tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err(format!("task {task_id} not cancellable")),
        }
    }

    pub(super) fn set_state(&self, id: &str, state: TaskState) {
        let mut reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        let mut keys = self.registry_keys.lock().unwrap_or_else(|e| e.into_inner());
        if !reg.contains_key(id) {
            keys.push_back(id.to_string());
            // Evict oldest entries when over the cap.
            while reg.len() >= MAX_REGISTRY_ENTRIES {
                if let Some(oldest) = keys.pop_front() {
                    reg.remove(&oldest);
                } else {
                    break;
                }
            }
        }
        reg.insert(id.to_string(), state);
    }

    pub fn get_state(&self, id: &str) -> Option<TaskState> {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    /// Unix timestamp of the last inbound task (`run_sync`/`start_async`).
    /// Returns 0 if no task has been handled yet.
    pub fn last_activity_at(&self) -> i64 {
        self.last_activity_at.load(Ordering::Relaxed)
    }
}
