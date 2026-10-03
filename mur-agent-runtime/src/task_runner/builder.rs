use super::*;

impl TaskRunner {
    pub fn new_stub_echo() -> Self {
        Self::with_backend(RunnerBackend::StubEcho)
    }

    pub fn new_stub_slow() -> Self {
        Self::with_backend(RunnerBackend::StubSlow)
    }

    /// Runner that replies to every turn with a misconfiguration notice instead
    /// of calling a model. Used when the configured provider has no client.
    pub fn new_stub_misconfigured(message: impl Into<String>) -> Self {
        Self::with_backend(RunnerBackend::Misconfigured(message.into()))
    }

    pub fn with_llm(client: Arc<dyn LlmClient>) -> Self {
        Self::with_backend(RunnerBackend::Llm(client))
    }

    /// A runner whose turns run inside a spawned CLI.
    ///
    /// Beside `with_llm` rather than derived from it: this backend has no
    /// `LlmClient` to hold. The CLI owns the loop, so there is no completion
    /// call for MUR to make.
    ///
    /// The socket is **not** optional in practice — without it the dispatch
    /// arm refuses rather than spawning a CLI that cannot reach MUR's tools —
    /// but it is set separately by `with_socket_path`, because the value
    /// comes from the profile's transport config and this constructor is
    /// called from places that have the backend before they have the socket.
    pub fn with_cli_spawn(b: &'static mur_common::cli_backend::CliBackend) -> Self {
        Self::with_backend(RunnerBackend::CliSpawn(b))
    }

    #[cfg(test)]
    pub(crate) fn backend_for_test(&self) -> &RunnerBackend {
        &self.backend
    }

    #[cfg(test)]
    pub(crate) fn socket_path_for_test(&self) -> Option<&std::path::Path> {
        self.socket_path.as_deref()
    }

    pub fn with_backend(backend: RunnerBackend) -> Self {
        Self {
            backend,
            registry: Arc::new(Mutex::new(HashMap::new())),
            registry_keys: Arc::new(Mutex::new(VecDeque::new())),
            cancel_signals: Arc::new(Mutex::new(HashMap::new())),
            telemetry: None,
            system_prompt: None,
            last_activity_at: Arc::new(AtomicI64::new(0)),
            skills: None,
            skills_cfg: SkillsConfig::default(),
            memory_cfg: Default::default(),
            recently_fired: Mutex::new(VecDeque::new()),
            turn_counter: AtomicU64::new(0),
            cumulative_input_tokens: AtomicU64::new(0),
            cumulative_output_tokens: AtomicU64::new(0),
            last_input_tokens: AtomicU64::new(0),
            last_model_ref: Mutex::new(None),
            last_turn_truncated: AtomicBool::new(false),
            hook_chain: None,
            hook_ctx: None,
            hook_cancel: None,
            pending_approvals: None,
            shim_trust: Default::default(),
            decision_store: None,
            agent_name: String::new(),
            notifier: None,
            client_notifiers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            approval_sinks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            steering: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            hitl_timeout_secs: 300,
            // Fail closed: an unset sandbox state refuses every `Ask` tool.
            // Production sets it from the B1 seal (`build_runner`).
            sandbox_enforcing: false,
            limits: (Default::default(), None),
            iteration_ceiling: ITERATION_CEILING,
            autonomy: mur_common::hitl::Autonomy::default(),
            tools: vec![],
            tools_policy: vec![],
            socket_path: None,
            secrets: None,
            scratch_dir: None,
            bash_jobs: None,
            effort: std::sync::RwLock::new(None),
            conversations: Mutex::new(ConversationStore::default()),
            session_cwd: None,
            project_instructions: None,
            draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal this runner to stop accepting new turns. Any turn that arrives
    /// after `begin_drain()` returns a transient `TaskOutcome::Failed` so the
    /// caller can retry after the runtime restarts. In-flight turns are NOT
    /// aborted; they complete normally.
    pub fn begin_drain(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    /// Returns `true` when no task is in `TaskState::Working`, or `false`
    /// if `timeout` elapses first.
    ///
    /// The registry retains completed/failed/cancelled entries, so the length
    /// is NOT a reliable idle indicator. Instead this polls for the absence of
    /// any `TaskState::Working` entry — which is inserted before a turn begins
    /// and transitioned to a terminal state when it ends (or is cancelled).
    /// Polls every 50 ms to stay responsive under a typical stop_timeout_secs.
    pub async fn await_idle(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            {
                let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
                let working = reg.values().any(|s| matches!(s, TaskState::Working));
                if !working {
                    return true;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn crate::tools::ToolExecutor>>) -> Self {
        self.tools = tools;
        self
    }

    /// The agent's own socket, so a spawned CLI's shim can dial back in.
    ///
    /// Carried rather than derived: the runner has no profile, and the bound
    /// path is not always the canonical one — `socket_path::resolve_bind_target`
    /// relocates a long path to /tmp and symlinks it.
    pub fn with_socket_path(mut self, p: std::path::PathBuf) -> Self {
        self.socket_path = Some(p);
        self
    }

    pub fn with_tools_policy(mut self, rules: Vec<mur_common::agent::ToolRule>) -> Self {
        self.tools_policy = rules;
        self
    }

    pub fn with_secrets(mut self, vault: Arc<crate::secrets::SecretVault>) -> Self {
        self.secrets = Some(vault);
        self
    }

    /// The agent's granted scratch dir, named in the system prompt.
    pub fn with_scratch_dir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.scratch_dir = dir;
        self
    }

    /// The bash job table (spec D3/D8), so the loop can end a task's jobs on
    /// an unattended stop or a cancel, and the supervisor every job at exit.
    pub fn with_bash_jobs(mut self, jobs: Arc<crate::tools::bash_jobs::JobTable>) -> Self {
        self.bash_jobs = Some(jobs);
        self
    }

    /// Runtime shutdown: every running bash job, whoever started it.
    pub async fn kill_all_jobs(&self) -> usize {
        match &self.bash_jobs {
            Some(t) => t.kill_all().await,
            None => 0,
        }
    }

    pub(super) async fn kill_jobs_of(&self, task_id: &str) {
        if let Some(t) = &self.bash_jobs {
            let n = t.kill_owned_by(task_id).await;
            if n > 0 {
                tracing::info!(task_id, jobs = n, "ended the task's running bash jobs");
            }
        }
    }

    /// Set the agent's per-turn effort (from its profile) at construction.
    pub fn with_effort(mut self, effort: Option<mur_common::llm::Effort>) -> Self {
        self.effort = std::sync::RwLock::new(effort);
        self
    }

    /// The effort this agent's own turns currently request.
    ///
    /// A poisoned lock reports `None` rather than panicking: losing the
    /// session's effort override costs a slightly different reasoning budget,
    /// while panicking here would take down a running agent mid-turn.
    pub fn effort(&self) -> Option<mur_common::llm::Effort> {
        self.effort.read().map(|g| *g).unwrap_or(None)
    }

    /// Change the effort on a running agent (murmur `/effort`, via the
    /// `effort/set` A2A method). Takes effect on the next request.
    pub fn set_effort(&self, effort: Option<mur_common::llm::Effort>) {
        if let Ok(mut g) = self.effort.write() {
            *g = effort;
        }
    }

    pub fn with_telemetry(mut self, tx: mpsc::Sender<Event>) -> Self {
        self.telemetry = Some(tx);
        self
    }

    pub fn with_system_prompt(mut self, prompt: Option<String>) -> Self {
        self.system_prompt = prompt;
        self
    }

    pub fn with_skills(mut self, skills: Arc<RuntimeSkills>) -> Self {
        self.skills = Some(skills);
        self
    }

    pub fn with_memory_cfg(mut self, cfg: mur_common::config::MemoryConfig) -> Self {
        self.memory_cfg = cfg;
        self
    }

    pub fn with_skills_cfg(mut self, cfg: SkillsConfig) -> Self {
        self.skills_cfg = cfg;
        self
    }

    pub fn with_hook_chain(
        mut self,
        chain: Arc<HookChain>,
        ctx: HookCtx,
        cancel: CancellationToken,
    ) -> Self {
        self.hook_chain = Some(chain);
        self.hook_ctx = Some(ctx);
        self.hook_cancel = Some(cancel);
        self
    }

    /// The registry `shim/hello` and `tool/hitl_respond` consult. Owned here
    /// because this is where tickets are issued and where the gate runs.
    pub fn shim_trust(&self) -> crate::hitl::shim_ticket::ShimTrust {
        self.shim_trust.clone()
    }

    pub fn with_pending_approvals(mut self, pa: HitlApprovals) -> Self {
        self.pending_approvals = Some(pa);
        self
    }

    /// Persist multi-turn memory under `dir` so conversations survive a restart
    /// (issue #1199), and size stored history against `context_window` rather
    /// than a turn count (issue #1200). `None` window keeps the default budget.
    pub fn with_conversation_memory(
        self,
        dir: std::path::PathBuf,
        context_window: Option<u64>,
    ) -> Self {
        {
            let mut store = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
            store.dir = Some(dir);
            if let Some(w) = context_window.filter(|w| *w > 0) {
                store.budget_tokens = (w / CONV_BUDGET_DIVISOR).max(1);
            }
        }
        self
    }

    /// Share the tools' session cwd so each turn can adopt the caller's
    /// working directory and declare it in the prompt. `allowed_roots` are the
    /// entitlement roots (read + write + agent home) a `TaskSpec.cwd` must fall
    /// under to be adopted — a client cannot point the tools somewhere the
    /// profile never granted.
    pub fn with_session_cwd(
        mut self,
        cwd: crate::tools::fs_policy::SessionCwd,
        allowed_roots: Vec<String>,
    ) -> Self {
        self.session_cwd = Some((cwd, allowed_roots));
        self
    }

    /// Load the session cwd's project instruction files into every turn's
    /// system prompt (see [`crate::project_instructions`]). Inert without
    /// [`Self::with_session_cwd`]: no working directory, no project.
    pub fn with_project_instructions(
        mut self,
        p: crate::project_instructions::ProjectInstructions,
    ) -> Self {
        self.project_instructions = Some(p);
        self
    }

    /// P3: where settled chat-gate decisions are looked up and recorded.
    pub fn with_decision_store(mut self, s: Arc<dyn crate::hitl::store::DecisionStore>) -> Self {
        self.decision_store = Some(s);
        self
    }

    /// The agent's own name — part of every chat-gate hash.
    /// The canonical agent name this runner hosts (empty on stub runners).
    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    /// The tools a brief declares it needs that this runtime cannot offer:
    /// not registered, or denied by policy — the same inventory and the same
    /// rule list the gate consults, so preflight and gate cannot disagree
    /// (spec §3.8).
    pub fn missing_tools(&self, needs: &[String]) -> Vec<String> {
        needs
            .iter()
            .filter(|n| {
                !self.tools.iter().any(|t| t.name() == n.as_str())
                    || effective_tool_policy(&self.tools_policy, n)
                        == mur_common::agent::ToolPolicy::Deny
            })
            .cloned()
            .collect()
    }

    pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    pub fn with_notifier(mut self, tx: tokio::sync::mpsc::Sender<serde_json::Value>) -> Self {
        self.notifier = Some(tx);
        self
    }

    /// Register the connection sink that should receive this turn's HITL
    /// approval prompts, keyed by the turn's task id.
    /// `can_approve` says whether a human is reading this connection and able
    /// to answer an approval prompt. `false` makes the gate deny at once
    /// instead of waiting out `hitl.timeout_secs` for nobody.
    pub async fn register_client_notifier(
        &self,
        task_id: &str,
        tx: tokio::sync::mpsc::Sender<serde_json::Value>,
        can_approve: bool,
    ) {
        self.client_notifiers
            .lock()
            .await
            .insert(task_id.to_string(), (tx.clone(), can_approve));
        // Both, because for an in-process turn the attached client is the
        // audience and the approver. They only diverge for a spawned CLI.
        self.approval_sinks
            .lock()
            .await
            .insert(task_id.to_string(), (tx, can_approve));
    }

    /// Take this task's approval sink over for the duration of one call,
    /// handing back whatever was there so the caller can put it back.
    ///
    /// A CLI-spawn turn registers its client's sink under the turn's task id
    /// and then hands that SAME id to the shim, so the shim's `tools/call`
    /// arrives keyed on a task that already has an owner. Replacing it and
    /// then deleting it left the spawning turn unable to reach its client,
    /// and any later approval for that turn failing closed with a human
    /// sitting right there. Borrowing makes the nesting explicit instead of
    /// making the inner caller the last writer.
    pub async fn borrow_approval_sink(
        &self,
        task_id: &str,
        tx: tokio::sync::mpsc::Sender<serde_json::Value>,
        can_approve: bool,
    ) -> Option<ApprovalSink> {
        self.approval_sinks
            .lock()
            .await
            .insert(task_id.to_string(), (tx, can_approve))
    }

    /// Put back what `borrow_client_notifier` displaced, or clear the slot if
    /// it was empty before. Restoring `None` by removing is the point: the
    /// borrower must not leave its own sink behind after it has gone.
    pub async fn restore_approval_sink(&self, task_id: &str, prior: Option<ApprovalSink>) {
        let mut map = self.approval_sinks.lock().await;
        match prior {
            Some(p) => {
                map.insert(task_id.to_string(), p);
            }
            None => {
                map.remove(task_id);
            }
        }
    }

    /// Declare that nobody can answer an approval prompt for this turn.
    ///
    /// A delegated turn (`channel/delegate`) is synchronous — a fleet router is
    /// waiting on the reply — but "synchronous" is not "attended": the caller
    /// is a program. Registering nothing left the gate with no routed entry, so
    /// it fell through to asking and waited out `hitl.timeout_secs` against the
    /// agent-wide notifier that nobody was reading. The turn then returned with
    /// no agent message and nothing naming approval as the cause.
    ///
    /// The sink here is never written to: with `can_approve: false` every
    /// `Ask` call is decided by `decide_without_asking` before a prompt is
    /// built, so `pending` is always empty and the gate never runs. It exists
    /// only because the flag lives beside a sender in the same map.
    pub async fn mark_unattended(&self, task_id: &str) {
        // The receiver is dropped immediately: a send on this sink returns
        // `Err`, which is the correct fail-closed answer if a future change
        // ever routes a prompt here, and leaks nothing.
        let (tx, _) = tokio::sync::mpsc::channel(1);
        // The approval map only. Saying nobody can approve must not also say
        // nobody is watching — those became different statements.
        self.approval_sinks
            .lock()
            .await
            .insert(task_id.to_string(), (tx, false));
    }

    /// Drop the per-turn HITL sink once the turn completes.
    pub async fn unregister_client_notifier(&self, task_id: &str) {
        self.client_notifiers.lock().await.remove(task_id);
        self.approval_sinks.lock().await.remove(task_id);
    }

    /// Register a steering sender for the given task id.
    pub async fn register_steering(&self, task_id: &str, tx: tokio::sync::mpsc::Sender<String>) {
        self.steering.lock().await.insert(task_id.to_string(), tx);
    }

    /// Drop the steering sender once the turn completes.
    pub async fn unregister_steering(&self, task_id: &str) {
        self.steering.lock().await.remove(task_id);
    }

    /// Push a steering message to the running task; errors if no such task.
    pub async fn inject_steering(
        &self,
        task_id: &str,
        msg: String,
    ) -> Result<(), crate::protocol::a2a_server::HandlerError> {
        let tx = self.steering.lock().await.get(task_id).cloned();
        match tx {
            Some(tx) => tx.send(msg).await.map_err(|_| {
                crate::protocol::a2a_server::HandlerError::TaskNotFound(task_id.to_string())
            }),
            None => Err(crate::protocol::a2a_server::HandlerError::TaskNotFound(
                task_id.to_string(),
            )),
        }
    }

    pub fn with_hitl_timeout_secs(mut self, secs: u32) -> Self {
        self.hitl_timeout_secs = secs;
        self
    }

    /// Whether the B1 sandbox is enforcing for this process (D2 / D2b).
    pub fn with_sandbox_enforcing(mut self, enforcing: bool) -> Self {
        self.sandbox_enforcing = enforcing;
        self
    }

    /// The two scopes the runtime can see: `config.yaml limits:` and the
    /// agent's own `profile.yaml limits:`. Resolved per turn in `bounds_for`.
    pub fn with_limits(
        mut self,
        global: mur_common::limits::Limits,
        agent: Option<mur_common::limits::Limits>,
    ) -> Self {
        self.limits = (global, agent);
        self
    }

    /// What bounds this turn. Errors only when a file carries an unparsable
    /// value — surfaced as a failed task that names the key, per spec §4.
    pub(super) fn bounds_for(
        &self,
        spec: &TaskSpec,
    ) -> Result<crate::bounds::TurnBounds, TaskError> {
        crate::bounds::resolve_bounds(
            spec.attended,
            &self.limits.0,
            self.limits.1.as_ref(),
            spec.deadline_secs,
            std::time::Instant::now(),
        )
        .map_err(|e| task_error("limits", format!("limits: {e}"), false))
    }

    /// Issue #001: the turn-continuation policy, read from the agent's
    /// `hitl.autonomy`. Absent → `Autonomy::Ask`, the handback.
    pub fn with_autonomy(mut self, a: mur_common::hitl::Autonomy) -> Self {
        self.autonomy = a;
        self
    }

    /// Lower the diagnostic ceiling so a scripted stub loop ends. Production
    /// never calls this: the ceiling is not a setting (spec §6).
    #[cfg(test)]
    pub(crate) fn with_iteration_ceiling(mut self, n: u32) -> Self {
        self.iteration_ceiling = n;
        self
    }
}
