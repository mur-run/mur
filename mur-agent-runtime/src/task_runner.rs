//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx, PromptView};
use crate::llm::{LlmClient, LlmRequest, RichMessage};
use crate::skills::RuntimeSkills;
use crate::skills::injector::inject_layer2;
use crate::skills::trigger_matcher::{format_layer3, layer3_body, match_prompt};
use crate::telemetry_writer::{Event, SkillOutcome};
use mur_common::a2a::{Message, MessagePart, Task, TaskError, TaskState};
use mur_common::config::SkillsConfig;
use mur_common::skill::McpInventory;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub input: Message,
    pub context_task_id: Option<String>,
    /// Caller-supplied task id. When `Some`, the runner uses it verbatim so the
    /// client can cancel by an id it already holds; when `None` the runner
    /// generates one (back-compatible).
    pub task_id: Option<String>,
}

#[derive(Debug)]
pub enum TaskOutcome {
    Completed(Task),
    Failed(Task),
    Cancelled(Task),
}

#[derive(Clone)]
pub enum RunnerBackend {
    StubEcho,
    StubSlow,
    /// The agent's configured model provider has no client in this runtime
    /// (e.g. `deepseek`). Rather than silently echoing input — which looks
    /// alive but parrots — every turn replies with this misconfiguration
    /// message so the user sees exactly what to fix.
    Misconfigured(String),
    Llm(Arc<dyn LlmClient>),
}

/// Cap on task-state entries retained in memory. Oldest entries are evicted
/// when this limit is exceeded so long-lived agents don't leak unboundedly.
const MAX_REGISTRY_ENTRIES: usize = 1_024;

pub struct TaskRunner {
    backend: RunnerBackend,
    registry: Arc<Mutex<HashMap<String, TaskState>>>,
    /// Insertion-order index used to evict the oldest entry when `registry`
    /// exceeds `MAX_REGISTRY_ENTRIES`.
    registry_keys: Arc<Mutex<VecDeque<String>>>,
    cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    telemetry: Option<mpsc::Sender<Event>>,
    system_prompt: Option<String>,
    last_activity_at: Arc<AtomicI64>,
    skills: Option<Arc<RuntimeSkills>>,
    skills_cfg: SkillsConfig,
    recently_fired: Mutex<VecDeque<(u64, String)>>,
    turn_counter: AtomicU64,
    cumulative_input_tokens: AtomicU64,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
    pending_approvals: Option<HitlApprovals>,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    /// Per-turn client notifiers keyed by task id, registered by `message/send`
    /// so a tool-approval prompt is routed to the connection that issued the
    /// turn instead of broadcast to every client. Falls back to `notifier`.
    client_notifiers:
        Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::mpsc::Sender<serde_json::Value>>>>,
    hitl_timeout_secs: u32,
    max_iterations: u32,
    /// Per-task ceiling on cumulative input tokens for the agentic loop. The
    /// loop snapshots the (per-runner) counter at entry and stops gracefully
    /// once `current - start >= max_token_budget`.
    max_token_budget: u64,
    tools: Vec<Arc<dyn crate::tools::ToolExecutor>>,
    tools_policy: Vec<mur_common::agent::ToolRule>,
}

/// Default agentic-loop iteration cap when `HitlConfig.max_iterations` is unset.
/// A layered set of budgets (token + loop-detection) is the real safety net, so
/// this can be generous without risking runaway cost.
const DEFAULT_MAX_ITERATIONS: u32 = 25;

/// Default cumulative-input-token budget per task when `HitlConfig.max_tokens`
/// is unset. ≈ a few dollars on Sonnet; override per profile to bound spend.
const DEFAULT_MAX_TOKEN_BUDGET: u64 = 750_000;

/// Rolling-window size for doom-loop detection: the last N tool-call
/// fingerprints are retained.
const LOOP_WINDOW: usize = 8;

/// Number of identical tool-call fingerprints within the rolling window that
/// trips the doom-loop guard.
const LOOP_REPEAT_THRESHOLD: usize = 3;

/// Why the agentic loop stopped short of a natural end-turn. Carried out of the
/// loop into the task's `usage` JSON so callers can tell a clean completion from
/// a budget-truncated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopStop {
    MaxIterations,
    TokenBudget,
    LoopDetected,
}

impl LoopStop {
    fn as_str(self) -> &'static str {
        match self {
            LoopStop::MaxIterations => "max_iterations",
            LoopStop::TokenBudget => "token_budget",
            LoopStop::LoopDetected => "loop_detected",
        }
    }
}

/// An early, graceful termination of the agentic loop: which budget tripped and
/// how many iterations had completed. `None` (no `LoopExit`) means the model
/// ended the turn naturally.
#[derive(Debug, Clone, Copy)]
struct LoopExit {
    reason: LoopStop,
    iterations: u32,
}

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
            recently_fired: Mutex::new(VecDeque::new()),
            turn_counter: AtomicU64::new(0),
            cumulative_input_tokens: AtomicU64::new(0),
            hook_chain: None,
            hook_ctx: None,
            hook_cancel: None,
            pending_approvals: None,
            notifier: None,
            client_notifiers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            hitl_timeout_secs: 300,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_token_budget: DEFAULT_MAX_TOKEN_BUDGET,
            tools: vec![],
            tools_policy: vec![],
        }
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn crate::tools::ToolExecutor>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_tools_policy(mut self, rules: Vec<mur_common::agent::ToolRule>) -> Self {
        self.tools_policy = rules;
        self
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

    pub fn with_pending_approvals(mut self, pa: HitlApprovals) -> Self {
        self.pending_approvals = Some(pa);
        self
    }

    pub fn with_notifier(mut self, tx: tokio::sync::mpsc::Sender<serde_json::Value>) -> Self {
        self.notifier = Some(tx);
        self
    }

    /// Register the connection sink that should receive this turn's HITL
    /// approval prompts, keyed by the turn's task id.
    pub async fn register_client_notifier(
        &self,
        task_id: &str,
        tx: tokio::sync::mpsc::Sender<serde_json::Value>,
    ) {
        self.client_notifiers
            .lock()
            .await
            .insert(task_id.to_string(), tx);
    }

    /// Drop the per-turn HITL sink once the turn completes.
    pub async fn unregister_client_notifier(&self, task_id: &str) {
        self.client_notifiers.lock().await.remove(task_id);
    }

    pub fn with_hitl_timeout_secs(mut self, secs: u32) -> Self {
        self.hitl_timeout_secs = secs;
        self
    }

    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Set the per-task cumulative input-token budget for the agentic loop.
    pub fn with_max_token_budget(mut self, n: u64) -> Self {
        self.max_token_budget = n;
        self
    }

    fn assemble_system_prompt(&self, user_prompt: &str) -> (String, Vec<String>) {
        let base = self.system_prompt.clone().unwrap_or_default();
        let Some(skills) = &self.skills else {
            return (base, vec![]);
        };

        let turn = self
            .turn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let recently: HashSet<String> = {
            let q = self
                .recently_fired
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let horizon = turn.saturating_sub(
                self.skills_cfg
                    .adaptive
                    .as_ref()
                    .map(|a| a.recent_fire_boost_turns as u64)
                    .unwrap_or(0),
            );
            q.iter()
                .filter(|(t, _)| *t >= horizon)
                .map(|(_, n)| n.clone())
                .collect()
        };

        let ctx_fill = {
            let cumulative = self.cumulative_input_tokens.load(Ordering::Relaxed);
            let max = self
                .skills_cfg
                .adaptive
                .as_ref()
                .map(|a| a.model_max_context_tokens)
                .unwrap_or(200_000);
            if max == 0 {
                0.0
            } else {
                (cumulative as f64 / max as f64).clamp(0.0, 1.0)
            }
        };
        let injection = inject_layer2(&skills.loaded, &self.skills_cfg, ctx_fill, &recently);

        let triggered = match_prompt(&skills.triggers, user_prompt);

        let mut layer3 = String::new();
        let mut suppress_names: HashSet<&str> = HashSet::new();
        for t in &triggered {
            let Some(loaded) = skills.loaded.iter().find(|s| s.name == t.skill_name) else {
                continue;
            };
            let inventory = McpInventory::from_tool_names(
                self.tools.iter().map(|t| t.name().to_string()).collect(),
            );
            let Some(body) = layer3_body(&loaded.manifest, &inventory) else {
                continue;
            };
            layer3.push('\n');
            layer3.push_str(&format_layer3(&loaded.name, loaded.trust, &body));
            suppress_names.insert(loaded.name.as_str());
            {
                let mut q = self
                    .recently_fired
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                q.push_back((turn, loaded.name.clone()));
                // Prune entries that have fallen below the boost horizon so
                // the deque doesn't grow unboundedly on long-lived agents.
                let boost_turns = self
                    .skills_cfg
                    .adaptive
                    .as_ref()
                    .map(|a| a.recent_fire_boost_turns as u64)
                    .unwrap_or(0);
                let horizon = turn.saturating_sub(boost_turns);
                while q.front().map(|(t, _)| *t < horizon).unwrap_or(false) {
                    q.pop_front();
                }
            }
        }

        // Suppress Layer 2 lines for skills whose Layer 3 just loaded.
        let addendum = strip_lines_for(&injection.system_addendum, &suppress_names);

        let fired: Vec<String> = triggered.iter().map(|t| t.skill_name.clone()).collect();
        let mut combined = base;
        if !addendum.is_empty() {
            combined.push('\n');
            combined.push_str(&addendum);
        }
        if !layer3.is_empty() {
            combined.push('\n');
            combined.push_str(&layer3);
        }
        (combined, fired)
    }

    pub async fn run_sync(&self, spec: TaskSpec) -> TaskOutcome {
        self.run_sync_inner(spec, None).await
    }

    /// Like `run_sync`, but forwards each LLM token delta to `sink` as it is
    /// generated (used by message/send streaming). The final task is returned
    /// as usual once generation completes.
    pub async fn run_sync_streaming(
        &self,
        spec: TaskSpec,
        sink: tokio::sync::mpsc::Sender<crate::llm::StreamDelta>,
    ) -> TaskOutcome {
        self.run_sync_inner(spec, Some(sink)).await
    }

    async fn run_sync_inner(
        &self,
        spec: TaskSpec,
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
    ) -> TaskOutcome {
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

        let generation = async {
            match &self.backend {
                RunnerBackend::StubEcho => Ok((echo_response(&spec.input), None)),
                RunnerBackend::StubSlow => {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Ok((echo_response(&spec.input), None))
                }
                RunnerBackend::Misconfigured(message) => Ok((text_response(message), None)),
                RunnerBackend::Llm(client) => {
                    if self.pending_approvals.is_some() {
                        let system = self
                            .prepare_system_prompt(&spec.input)
                            .await
                            .unwrap_or_default();
                        self.run_agentic_loop(&id, client.as_ref(), system, &spec.input, sink)
                            .await
                    } else {
                        self.run_llm(&id, client.as_ref(), &spec.input, sink)
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
                    usage: None,
                });
            }
            Some(r) => r,
        };
        match result {
            Ok((reply, stop)) => {
                self.set_state(&id, TaskState::Completed);
                // When a budget forced an early, graceful exit, surface the
                // reason + iteration count in `usage` so callers can tell a
                // truncated completion from a natural one (the task still
                // reports Completed — work is preserved, not failed).
                let usage = stop.map(|exit| {
                    serde_json::json!({
                        "stop_reason": exit.reason.as_str(),
                        "iterations": exit.iterations,
                    })
                });
                TaskOutcome::Completed(Task {
                    id,
                    state: TaskState::Completed,
                    messages: vec![spec.input, reply],
                    created_at: now.clone(),
                    completed_at: Some(now),
                    error: None,
                    usage,
                })
            }
            Err(err) => {
                // A provider/runtime failure must surface as Failed with a
                // populated `error` — not a Completed task whose reply body
                // happens to contain "llm error:". Callers (message/send) and
                // the scheduler's `Failed` branch rely on this distinction.
                self.set_state(&id, TaskState::Failed);
                TaskOutcome::Failed(Task {
                    id,
                    state: TaskState::Failed,
                    messages: vec![spec.input],
                    created_at: now.clone(),
                    completed_at: Some(now),
                    error: Some(err),
                    usage: None,
                })
            }
        }
    }

    pub fn start_async(&self, spec: TaskSpec) -> AsyncTaskHandle {
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
        match tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err(format!("task {task_id} not cancellable")),
        }
    }

    fn set_state(&self, id: &str, state: TaskState) {
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

    async fn run_llm(
        &self,
        task_id: &str,
        client: &dyn LlmClient,
        input: &Message,
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
    ) -> Result<Message, TaskError> {
        let prompt = text_of(input);
        let mut messages: Vec<RichMessage> = Vec::new();

        let (system, fired) = self.assemble_system_prompt(&prompt);

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

        if !system.is_empty() {
            messages.push(RichMessage::Text {
                role: "system".into(),
                content: system.clone(),
            });
        }

        messages.push(RichMessage::Text {
            role: input.role.clone(),
            content: prompt.clone(),
        });
        let req = LlmRequest {
            messages,
            temperature: None,
            max_tokens: None,
            tools: vec![],
        };
        let start = std::time::Instant::now();
        let llm_result = match sink {
            Some(sink) => client.generate_stream(req, sink).await,
            None => client.generate(req).await,
        };
        match llm_result {
            Ok(resp) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                let _prev = self
                    .cumulative_input_tokens
                    .fetch_add(resp.input_tokens, Ordering::Relaxed);
                if let Some(tx) = &self.telemetry {
                    // Emit per-skill SkillExecuted events (M5a). Outcome is
                    // NotEvaluated because the M2 wiring doesn't track
                    // per-skill success/failure; the aggregator infers from
                    // LLM-call context.
                    let skill_events: Vec<Event> = fired
                        .iter()
                        .filter_map(|name| {
                            let loaded = self
                                .skills
                                .as_ref()
                                .and_then(|s| s.loaded.iter().find(|l| &l.name == name))?;
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

    fn tools_for_loop(&self) -> &[Arc<dyn crate::tools::ToolExecutor>] {
        &self.tools
    }

    async fn prepare_system_prompt(&self, input: &Message) -> Result<String, TaskError> {
        let prompt = text_of(input);
        let (system, _fired) = self.assemble_system_prompt(&prompt);
        if let (Some(chain), Some(ctx), Some(cancel)) =
            (&self.hook_chain, &self.hook_ctx, &self.hook_cancel)
        {
            let _ = (chain, ctx, cancel);
        }
        Ok(system)
    }

    async fn handle_tool_call(
        &self,
        task_id: &str,
        call: &crate::llm::ToolCallResult,
    ) -> Result<crate::llm::ToolResultEntry, TaskError> {
        use crate::llm::ToolResultEntry;

        // 1. Find tool — if no matching tool, return unknown-tool result immediately (skip HITL)
        let tool = self
            .tools_for_loop()
            .iter()
            .find(|t| t.name() == call.tool_name)
            .cloned();

        if tool.is_none() {
            return Ok(ToolResultEntry {
                call_id: call.call_id.clone(),
                content: format!("unknown tool: {}", call.tool_name),
                is_error: true,
            });
        }

        // 1b. Policy gate: check before executing.
        {
            use mur_common::agent::{ToolPolicy, resolve_tool_policy};
            match resolve_tool_policy(&self.tools_policy, &call.tool_name) {
                ToolPolicy::Deny => {
                    return Ok(ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: format!("Tool `{}` is denied by policy.", call.tool_name),
                        is_error: true,
                    });
                }
                ToolPolicy::Allow => {
                    // Execute without HITL gate below.
                    let tool = tool.unwrap();
                    let (output, is_error) = match tool.execute(call.input.clone()).await {
                        Ok(out) => (out, false),
                        Err(e) => (format!("tool error: {e}"), true),
                    };
                    return Ok(ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: output,
                        is_error,
                    });
                }
                ToolPolicy::Ask => {}
            }
        }

        // 2. Execute the tool
        let tool = tool.unwrap();
        let (output, is_error) = match tool.execute(call.input.clone()).await {
            Ok(out) => (out, false),
            Err(e) => (format!("tool error: {e}"), true),
        };

        // 3. HITL gate (only after tool execution). Route the approval prompt to
        // the connection that issued this turn (looked up by task id), falling
        // back to the baked notifier — never broadcast to other clients.
        let routed = self.client_notifiers.lock().await.get(task_id).cloned();
        let effective_notifier = routed.as_ref().or(self.notifier.as_ref());
        let decision =
            if let (Some(pa), Some(notifier)) = (&self.pending_approvals, effective_notifier) {
                let hitl_id = uuid::Uuid::now_v7().to_string();
                let (tx, rx) = tokio::sync::oneshot::channel::<crate::hitl::HitlDecision>();
                pa.lock().await.insert(hitl_id.clone(), tx);
                let notification = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "tool/approval_needed",
                    "params": {
                        "hitl_id": hitl_id,
                        "task_id": task_id,
                        "tool_name": call.tool_name,
                        "tool_input": call.input,
                        "output": output,
                        "is_error": is_error,
                        "timeout_ms": (self.hitl_timeout_secs as u64) * 1000,
                    }
                });
                let _ = notifier.send(notification).await;
                match tokio::time::timeout(
                    std::time::Duration::from_secs(self.hitl_timeout_secs as u64),
                    rx,
                )
                .await
                {
                    Ok(Ok(d)) => d,
                    _ => {
                        pa.lock().await.remove(&hitl_id);
                        crate::hitl::HitlDecision {
                            allow: false,
                            reason: Some("timed out".into()),
                        }
                    }
                }
            } else {
                crate::hitl::HitlDecision {
                    allow: true,
                    reason: None,
                }
            };

        if !decision.allow {
            let reason_str = decision.reason.as_deref().unwrap_or("denied");
            return Err(task_error(
                "hitl_denied",
                format!("tool call denied: {reason_str}"),
                false,
            ));
        }

        Ok(ToolResultEntry {
            call_id: call.call_id.clone(),
            content: output,
            is_error,
        })
    }

    /// Run the agentic loop. Returns the final agent message plus an optional
    /// `LoopStop` describing which budget (if any) forced an early, graceful
    /// exit. `None` means the model ended the turn naturally.
    async fn run_agentic_loop(
        &self,
        task_id: &str,
        client: &dyn crate::llm::LlmClient,
        system_prompt: String,
        input: &Message,
        _sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
    ) -> Result<(Message, Option<LoopExit>), TaskError> {
        use crate::llm::{LlmRequest, RichMessage, StopReason};

        let tool_defs: Vec<_> = self.tools_for_loop().iter().map(|t| t.def()).collect();
        let prompt = text_of(input);
        let mut history: Vec<RichMessage> = vec![
            RichMessage::Text {
                role: "system".into(),
                content: system_prompt,
            },
            RichMessage::Text {
                role: input.role.clone(),
                content: prompt,
            },
        ];

        // Snapshot the (per-runner, shared-across-tasks) token counter so the
        // budget measures THIS task's spend, not the runner's lifetime total.
        let start_tokens = self
            .cumulative_input_tokens
            .load(std::sync::atomic::Ordering::Relaxed);
        // Rolling window of recent tool-call fingerprints for doom-loop
        // detection: (tool_name, hash(canonical args), hash(result content)).
        // Keying on the RESULT too means a command repeated with identical
        // output (genuinely stuck) trips the guard, while the same command
        // returning changing output (e.g. `cargo build` between edits) does
        // NOT — that's progress, not a loop. Requires fingerprinting AFTER
        // the tool runs, since the result isn't known until then.
        let mut fingerprints: VecDeque<(String, u64, u64)> = VecDeque::with_capacity(LOOP_WINDOW);

        let mut iteration: u32 = 0;
        while iteration < self.max_iterations {
            // Token budget (primary control): stop before spending more once
            // this task's cumulative input tokens cross the ceiling.
            let spent = self
                .cumulative_input_tokens
                .load(std::sync::atomic::Ordering::Relaxed)
                .saturating_sub(start_tokens);
            if spent >= self.max_token_budget {
                let msg = self
                    .graceful_exit(client, &history, LoopStop::TokenBudget)
                    .await;
                return Ok((
                    msg,
                    Some(LoopExit {
                        reason: LoopStop::TokenBudget,
                        iterations: iteration,
                    }),
                ));
            }

            let req = LlmRequest {
                messages: history.clone(),
                temperature: None,
                max_tokens: None,
                tools: tool_defs.clone(),
            };
            let resp = client
                .generate(req)
                .await
                .map_err(|e| task_error("llm_error", format!("{e}"), true))?;

            self.cumulative_input_tokens
                .fetch_add(resp.input_tokens, std::sync::atomic::Ordering::Relaxed);

            // Truncation guard: a turn that hit the output-token ceiling AND
            // carries tool_calls was cut off MID-tool_use — the tool_use
            // `input` JSON is incomplete, so executing it (or appending it to
            // history and re-looping) just replays a malformed call and trips
            // the doom-loop guard. Instead of running the broken call, append a
            // user-role guidance message and continue so the model can recover
            // with a shorter, well-formed turn. This counts toward the
            // iteration budget; the iteration cap + doom-loop guard remain the
            // backstops against a model that truncates forever.
            if resp.stop_reason == StopReason::MaxTokens && !resp.tool_calls.is_empty() {
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

            history.push(RichMessage::ToolUse {
                text: if resp.text.is_empty() {
                    None
                } else {
                    Some(resp.text.clone())
                },
                calls: resp.tool_calls.clone(),
            });

            if resp.tool_calls.is_empty() || resp.stop_reason == StopReason::EndTurn {
                return Ok((
                    Message {
                        role: "agent".into(),
                        parts: vec![mur_common::a2a::MessagePart::Text { text: resp.text }],
                    },
                    None,
                ));
            }

            // Execute tools and collect results.
            let mut results = Vec::new();
            for call in &resp.tool_calls {
                match self.handle_tool_call(task_id, call).await {
                    Ok(entry) => results.push(entry),
                    Err(e) => return Err(e),
                }
            }

            // Doom-loop detection (safety layer): fingerprint every tool call
            // by (tool, args, RESULT) and abort if any single fingerprint
            // repeats `LOOP_REPEAT_THRESHOLD` times within the rolling window.
            // Keying on the result is the crux: "same command + same output,
            // repeated" = stuck (abort); "same command, changing output" =
            // making progress (don't abort). Fingerprinting happens here,
            // AFTER execution, because the result is needed.
            for (call, entry) in resp.tool_calls.iter().zip(results.iter()) {
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
                if repeats >= LOOP_REPEAT_THRESHOLD {
                    // Append the results gathered this turn before exiting so
                    // the dangling tool_use is closed; graceful_exit also
                    // sanitizes, but keeping history consistent is cheap.
                    history.push(RichMessage::ToolResults { results });
                    let msg = self
                        .graceful_exit(client, &history, LoopStop::LoopDetected)
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

            history.push(RichMessage::ToolResults { results });
            iteration += 1;
        }

        let msg = self
            .graceful_exit(client, &history, LoopStop::MaxIterations)
            .await;
        Ok((
            msg,
            Some(LoopExit {
                reason: LoopStop::MaxIterations,
                iterations: iteration,
            }),
        ))
    }

    /// One final, tools-DISABLED LLM turn asking the model to summarize what it
    /// completed, the current build/test state, and the remaining steps. If that
    /// call fails, fall back to the last assistant text already in `history` so
    /// accumulated work is never lost.
    async fn graceful_exit(
        &self,
        client: &dyn crate::llm::LlmClient,
        history: &[crate::llm::RichMessage],
        reason: LoopStop,
    ) -> Message {
        use crate::llm::{LlmRequest, RichMessage};

        let nudge = format!(
            "You have hit the {} budget for this task. Stop calling tools. \
             Summarize what you completed, the current build/test state, and the \
             remaining steps so work can resume later.",
            reason.as_str()
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
            tools: vec![], // tools disabled: force a textual summary
        };
        match client.generate(req).await {
            Ok(resp) => {
                self.cumulative_input_tokens
                    .fetch_add(resp.input_tokens, std::sync::atomic::Ordering::Relaxed);
                Message {
                    role: "agent".into(),
                    parts: vec![mur_common::a2a::MessagePart::Text { text: resp.text }],
                }
            }
            Err(_) => {
                // Never lose work: return the last assistant text from history.
                let text = last_assistant_text(history)
                    .unwrap_or_else(|| format!("Stopped: {} budget reached.", reason.as_str()));
                Message {
                    role: "agent".into(),
                    parts: vec![mur_common::a2a::MessagePart::Text { text }],
                }
            }
        }
    }
}

/// Deterministic hash of a tool call's arguments. Serializes to canonical JSON
/// (sorted keys via `serde_json::Value`'s BTreeMap-backed object) before hashing
/// so logically-identical args always fingerprint the same.
fn fingerprint_args(args: &serde_json::Value) -> u64 {
    fingerprint_str(&args.to_string())
}

/// Deterministic hash of an arbitrary string (used for canonical tool-result
/// content in the doom-loop fingerprint). `DefaultHasher` is fixed-seed, so the
/// same input always yields the same value within a build — sufficient for
/// equality-within-window comparison, no randomness.
fn fingerprint_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Return a copy of `history` in which every `ToolUse` call is guaranteed to be
/// followed by a matching `tool_result`. For any `tool_use` id not covered by
/// the message immediately after its `ToolUse` turn, a synthetic
/// `ToolResults` entry is inserted right after that turn with content
/// `"[stopped: <reason>]"`. This keeps the resulting `LlmRequest` well-formed so
/// the Anthropic API does not reject it with "tool_use ids without tool_result".
fn sanitize_dangling_tool_uses(
    history: &[crate::llm::RichMessage],
    reason: LoopStop,
) -> Vec<crate::llm::RichMessage> {
    use crate::llm::{RichMessage, ToolResultEntry};

    let mut out: Vec<RichMessage> = Vec::with_capacity(history.len() + 1);
    for (i, msg) in history.iter().enumerate() {
        out.push(msg.clone());
        if let RichMessage::ToolUse { calls, .. } = msg {
            // Ids the very next message already answers (the API requires the
            // tool_result block to come immediately after the tool_use turn).
            let covered: HashSet<&str> = match history.get(i + 1) {
                Some(RichMessage::ToolResults { results }) => {
                    results.iter().map(|r| r.call_id.as_str()).collect()
                }
                _ => HashSet::new(),
            };
            let missing: Vec<ToolResultEntry> = calls
                .iter()
                .filter(|c| !covered.contains(c.call_id.as_str()))
                .map(|c| ToolResultEntry {
                    call_id: c.call_id.clone(),
                    content: format!("[stopped: {}]", reason.as_str()),
                    is_error: true,
                })
                .collect();
            if !missing.is_empty() {
                out.push(RichMessage::ToolResults { results: missing });
            }
        }
    }
    out
}

/// Best-effort recovery of the most recent assistant-authored text from the
/// loop history (the inline reasoning attached to a tool-use turn).
fn last_assistant_text(history: &[crate::llm::RichMessage]) -> Option<String> {
    use crate::llm::RichMessage;
    history.iter().rev().find_map(|m| match m {
        RichMessage::ToolUse { text: Some(t), .. } if !t.is_empty() => Some(t.clone()),
        RichMessage::Text { role, content } if role == "agent" || role == "assistant" => {
            Some(content.clone())
        }
        _ => None,
    })
}

/// Build a `TaskError` for a failed task outcome.
fn task_error(code: &str, message: String, recoverable: bool) -> TaskError {
    TaskError {
        code: code.to_string(),
        message,
        recoverable,
        details: None,
    }
}

fn text_of(m: &Message) -> String {
    m.parts
        .iter()
        .find_map(|p| match p {
            MessagePart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn strip_lines_for(text: &str, names: &HashSet<&str>) -> String {
    if names.is_empty() {
        return text.to_string();
    }
    text.lines()
        .filter(|line| {
            !names
                .iter()
                .any(|n| line.contains(&format!("[Skill: {n} ")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct AsyncTaskHandle {
    id: String,
    done: oneshot::Receiver<TaskOutcome>,
}

impl AsyncTaskHandle {
    pub fn task_id(&self) -> &str {
        &self.id
    }

    pub async fn await_completion(self) -> TaskOutcome {
        self.done.await.unwrap_or_else(|_| {
            TaskOutcome::Failed(Task {
                id: self.id,
                state: TaskState::Failed,
                messages: vec![],
                created_at: chrono::Utc::now().to_rfc3339(),
                completed_at: None,
                error: None,
                usage: None,
            })
        })
    }
}

fn echo_response(input: &Message) -> Message {
    let text = input
        .parts
        .iter()
        .find_map(|p| match p {
            MessagePart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Message {
        role: "agent".into(),
        parts: vec![MessagePart::Text {
            text: format!("echo: {text}"),
        }],
    }
}

/// Build a plain agent reply carrying `text` verbatim (no model call).
fn text_response(text: &str) -> Message {
    Message {
        role: "agent".into(),
        parts: vec![MessagePart::Text { text: text.into() }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::a2a::MessagePart;

    fn ping_spec() -> TaskSpec {
        TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![MessagePart::Text {
                    text: "ping".into(),
                }],
            },
            context_task_id: None,
            task_id: None,
        }
    }

    #[tokio::test]
    async fn last_activity_starts_at_zero() {
        let runner = TaskRunner::new_stub_echo();
        assert_eq!(runner.last_activity_at(), 0);
    }

    #[tokio::test]
    async fn start_async_bumps_last_activity() {
        let runner = TaskRunner::new_stub_echo();
        let before = chrono::Utc::now().timestamp();
        let _handle = runner.start_async(ping_spec());
        let activity = runner.last_activity_at();
        let after = chrono::Utc::now().timestamp();
        assert!(
            activity >= before && activity <= after,
            "activity={activity} not in [{before},{after}]"
        );
    }

    #[tokio::test]
    async fn run_sync_bumps_last_activity() {
        // Regression: the production inbound path must record activity so idle
        // triggers measure real quiescence (previously only start_async did).
        let runner = TaskRunner::new_stub_echo();
        let before = chrono::Utc::now().timestamp();
        let _ = runner.run_sync(ping_spec()).await;
        let activity = runner.last_activity_at();
        let after = chrono::Utc::now().timestamp();
        assert!(
            activity >= before && activity <= after,
            "activity={activity} not in [{before},{after}]"
        );
    }

    #[test]
    fn task_spec_accepts_optional_task_id() {
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![MessagePart::Text { text: "hi".into() }],
            },
            context_task_id: None,
            task_id: Some("task-fixed-1".to_string()),
        };
        assert_eq!(spec.task_id.as_deref(), Some("task-fixed-1"));
    }

    #[tokio::test]
    async fn run_sync_uses_supplied_task_id() {
        let runner = TaskRunner::new_stub_echo();
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![MessagePart::Text { text: "hi".into() }],
            },
            context_task_id: None,
            task_id: Some("task-supplied-9".to_string()),
        };
        let outcome = runner.run_sync(spec).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed")
        };
        assert_eq!(task.id, "task-supplied-9");
    }

    #[tokio::test]
    async fn run_sync_streaming_is_cancellable_by_id() {
        use std::sync::Arc;
        let runner = Arc::new(TaskRunner::new_stub_slow());
        let (tx, _rx) = tokio::sync::mpsc::channel(8); // streaming sink, unused here
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![MessagePart::Text {
                    text: "slow".into(),
                }],
            },
            context_task_id: None,
            task_id: Some("task-cancelme".to_string()),
        };
        let r2 = runner.clone();
        let handle = tokio::spawn(async move { r2.run_sync_streaming(spec, tx).await });

        // Let the task register its cancel signal, then cancel by the known id.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        runner
            .cancel("task-cancelme")
            .await
            .expect("cancel should succeed");

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("must finish promptly, not wait 60s")
            .expect("join");
        let TaskOutcome::Cancelled(task) = outcome else {
            panic!("expected Cancelled, got {outcome:?}")
        };
        assert_eq!(task.id, "task-cancelme");
        assert_eq!(task.state, TaskState::Cancelled);
    }

    #[tokio::test]
    async fn run_sync_llm_error_yields_failed() {
        // Regression: a provider failure must surface as Failed with a
        // populated error, not a Completed task whose body says "llm error:".
        use crate::llm::stub::StubLlm;
        let yaml = r#"
- match: { contains: "ping" }
  fault: rate_limit
"#;
        let client = std::sync::Arc::new(StubLlm::from_yaml(yaml).unwrap());
        let runner = TaskRunner::with_llm(client);
        let outcome = runner.run_sync(ping_spec()).await;
        match outcome {
            TaskOutcome::Failed(task) => {
                assert_eq!(task.state, TaskState::Failed);
                let err = task.error.expect("Failed task must carry an error");
                assert_eq!(err.code, "llm_error");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // Helper: build LlmResponse that signals tool_use stop with one call
    fn tool_call_response(call_id: &str, command: &str) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: call_id.into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": command}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        }
    }

    /// Like `tool_call_response` but with a caller-specified input-token count,
    /// for exercising the token budget.
    fn tool_call_response_tokens(
        call_id: &str,
        command: &str,
        input_tokens: u64,
    ) -> crate::llm::LlmResponse {
        let mut r = tool_call_response(call_id, command);
        r.input_tokens = input_tokens;
        r
    }

    fn end_turn_response(text: &str) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: text.into(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![],
            stop_reason: crate::llm::StopReason::EndTurn,
        }
    }

    /// A response truncated mid-tool_use: `stop_reason == MaxTokens` while a
    /// tool_call is present. The `input` is the empty `{}` that
    /// `parse_response_body` yields when the assistant turn was cut off before
    /// the tool_use JSON finished — i.e. the malformed call the loop must NOT
    /// execute blindly.
    fn truncated_tool_call_response(call_id: &str) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: call_id.into(),
                tool_name: "bash".into(),
                // Empty input — the hallmark of a truncated tool_use.
                input: serde_json::json!({}),
            }],
            stop_reason: crate::llm::StopReason::MaxTokens,
        }
    }

    /// Counting `bash` tool: records how many times it executes so a test can
    /// assert a (truncated) tool call was NOT run.
    struct CountingBashTool {
        calls: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for CountingBashTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "bash".into(),
                description: "test bash tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<String, crate::tools::ToolError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok("ran".into())
        }
    }

    /// Fix B — truncation is self-correcting, not a silent loop. When a turn
    /// stops with `MaxTokens` AND carries tool_calls (cut off mid-tool_use),
    /// the loop must NOT execute the malformed call. Instead it appends a
    /// truncation-guidance user message and continues, letting the model
    /// recover with a shorter, well-formed turn.
    #[tokio::test]
    async fn truncated_tool_use_injects_guidance_and_recovers() {
        use crate::llm::stub::SequenceLlm;
        // Turn 0: truncated mid-tool_use (MaxTokens + a tool_call).
        // Turn 1: a clean end-turn — the recovery the model produces after the
        // guidance nudge.
        let responses: Vec<crate::llm::LlmResponse> = vec![
            truncated_tool_call_response("trunc-0"),
            end_turn_response("RECOVERED: produced a shorter response."),
        ];
        let calls = Arc::new(AtomicU64::new(0));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(CountingBashTool {
                    calls: calls.clone(),
                })])
                .with_tools_policy(vec![mur_common::agent::ToolRule {
                    pattern: "bash".into(),
                    policy: mur_common::agent::ToolPolicy::Allow,
                    risk: None,
                }])
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(50),
        );
        let outcome = runner.run_sync(loop_spec("truncate")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (recovered turn), got {outcome:?}");
        };
        // The malformed (truncated) tool call must NOT have been executed.
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "truncated tool_use must not be executed"
        );
        // The following well-formed turn proceeds and is the natural terminus —
        // no budget tripped, so usage is None (natural end_turn).
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("RECOVERED"),
            "expected the recovery turn's reply, got: {reply_text}"
        );
        assert!(
            task.usage.is_none(),
            "natural end_turn after recovery must not populate a budget stop_reason; usage={:?}",
            task.usage
        );
    }

    #[tokio::test]
    async fn loop_ends_on_end_turn_no_tools() {
        use crate::llm::stub::SequenceLlm;
        let llm = SequenceLlm::new(vec![end_turn_response("Completed.")]);
        let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
        let pa: Arc<
            tokio::sync::Mutex<
                HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>,
            >,
        > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(llm))
                .with_pending_approvals(pa)
                .with_notifier(notif_tx),
        );
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text {
                    text: "hello".into(),
                }],
            },
            context_task_id: None,
            task_id: None,
        };
        let outcome = runner.run_sync(spec).await;
        assert!(matches!(outcome, TaskOutcome::Completed(_)));
    }

    #[tokio::test]
    async fn max_iterations_exceeded_yields_completed_with_summary() {
        use crate::llm::stub::SequenceLlm;
        // Three tool_use turns fill the cap; the fourth call is the graceful,
        // tools-disabled summary turn. SequenceLlm wraps modulo len, so a
        // 4-element vector maps loop turns to indices 0,1,2 and the summary
        // turn to index 3 deterministically.
        // Distinct commands per turn so the doom-loop guard (identical-call
        // detection) does NOT fire first — this test must exercise the
        // iteration cap specifically.
        let responses: Vec<crate::llm::LlmResponse> = vec![
            tool_call_response("id-0", "echo step-0"),
            tool_call_response("id-1", "echo step-1"),
            tool_call_response("id-2", "echo step-2"),
            end_turn_response("SUMMARY: completed nothing; build untouched; remaining: all."),
        ];
        let llm = SequenceLlm::new(responses);
        let (notif_tx, _rx) = tokio::sync::mpsc::channel(16);
        let pa: Arc<
            tokio::sync::Mutex<
                HashMap<String, tokio::sync::oneshot::Sender<crate::hitl::HitlDecision>>,
            >,
        > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(llm))
                .with_pending_approvals(pa)
                .with_notifier(notif_tx)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(3),
        );
        let spec = TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text {
                    text: "loop".into(),
                }],
            },
            context_task_id: None,
            task_id: None,
        };
        let outcome = runner.run_sync(spec).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (graceful exit), got {outcome:?}");
        };
        // The returned reply carries the summarizing turn's text.
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("SUMMARY:"),
            "expected summary in reply, got: {reply_text}"
        );
        // The stop reason is surfaced in usage for callers to inspect.
        let usage = task.usage.expect("graceful exit must populate usage");
        assert_eq!(usage["stop_reason"], "max_iterations", "usage={usage}");
        assert_eq!(usage["iterations"], 3, "usage={usage}");
    }

    /// Step 3 (token budget): with a tiny `max_tokens` ceiling and an LLM that
    /// reports input tokens, the loop stops early — before the iteration cap —
    /// with stop_reason "token_budget", returning a Completed task carrying the
    /// summary. The budget is checked at loop entry (current - start), so the
    /// first turn always runs; the second turn trips the ceiling.
    #[tokio::test]
    async fn token_budget_exceeded_yields_completed_with_summary() {
        use crate::llm::stub::SequenceLlm;
        // idx0: one tool turn reporting 100 input tokens (>budget of 10).
        // idx1: the graceful, tools-disabled summary turn.
        let responses: Vec<crate::llm::LlmResponse> = vec![
            tool_call_response_tokens("id-0", "echo work", 100),
            end_turn_response("TOKEN SUMMARY: ran one step; build untouched; remaining: rest."),
        ];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                // High iteration cap so only the token budget can stop us.
                .with_max_iterations(50)
                .with_max_token_budget(10),
        );
        let outcome = runner.run_sync(loop_spec("budget")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (token-budget graceful exit), got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("TOKEN SUMMARY"),
            "expected summary in reply, got: {reply_text}"
        );
        let usage = task.usage.expect("token-budget exit must populate usage");
        assert_eq!(usage["stop_reason"], "token_budget", "usage={usage}");
        // Stopped after exactly one completed iteration, far below the cap of 50.
        assert_eq!(usage["iterations"], 1, "usage={usage}");
    }

    /// Step 4 (doom-loop detection): an LLM that emits the SAME tool call every
    /// turn must be aborted after ~3 identical calls with stop_reason
    /// "loop_detected" — well before the iteration cap (50 here). This catches
    /// blind identical retries quickly regardless of how high the cap is.
    #[tokio::test]
    async fn doom_loop_detected_yields_completed_with_summary() {
        use crate::llm::stub::SequenceLlm;
        // Identical args every turn. With no tool registered, every call
        // resolves to the same "unknown tool" result, so the full
        // (tool, args, RESULT) fingerprint is identical each turn. The 3rd
        // identical fingerprint trips the guard on iteration index 2; the 4th
        // call is the graceful summary.
        let responses: Vec<crate::llm::LlmResponse> = vec![
            tool_call_response("same-0", "echo identical"),
            tool_call_response("same-1", "echo identical"),
            tool_call_response("same-2", "echo identical"),
            end_turn_response("LOOP SUMMARY: stuck retrying; build untouched; need new approach."),
        ];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                // High cap so only doom-loop detection can stop us this fast.
                .with_max_iterations(50),
        );
        let outcome = runner.run_sync(loop_spec("doom")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (doom-loop graceful exit), got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("LOOP SUMMARY"),
            "expected summary in reply, got: {reply_text}"
        );
        let usage = task.usage.expect("doom-loop exit must populate usage");
        assert_eq!(usage["stop_reason"], "loop_detected", "usage={usage}");
        // Aborted within ~3 iterations, far below the cap of 50.
        let iters = usage["iterations"]
            .as_u64()
            .expect("iterations is a number");
        assert!(iters < 5, "expected early abort, got {iters} iterations");
    }

    /// Test LLM that records how many times `generate` was called and always
    /// emits a tool_use response (so the agentic loop never ends naturally).
    struct CountingToolLlm {
        calls: Arc<AtomicU64>,
        input_tokens_per_call: u64,
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmClient for CountingToolLlm {
        async fn generate(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(crate::llm::LlmResponse {
                text: String::new(),
                input_tokens: self.input_tokens_per_call,
                output_tokens: 1,
                model: "counting".into(),
                tool_calls: vec![crate::llm::ToolCallResult {
                    call_id: format!("id-{n}"),
                    tool_name: "bash".into(),
                    input: serde_json::json!({"command": "echo loop"}),
                }],
                stop_reason: crate::llm::StopReason::ToolUse,
            })
        }
        fn model_name(&self) -> &str {
            "counting"
        }
    }

    fn loop_spec(text: &str) -> TaskSpec {
        TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![mur_common::a2a::MessagePart::Text { text: text.into() }],
            },
            context_task_id: None,
            task_id: None,
        }
    }

    fn empty_pending_approvals() -> HitlApprovals {
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    /// Step 1 (config wiring): the PRODUCTION wiring function `build_runner`
    /// must honour a `max_iterations` of 3 — proving `HitlConfig.max_iterations`
    /// is threaded through, not just the test-only builder. The counting LLM
    /// emits tool_use every turn, so without the cap the loop would run forever
    /// (well, until the default). We assert generate() is called at most 3+1
    /// times (3 loop turns plus one graceful-summary turn).
    #[tokio::test]
    async fn build_runner_caps_loop_at_configured_max_iterations() {
        let calls = Arc::new(AtomicU64::new(0));
        let client: Arc<dyn crate::llm::LlmClient> = Arc::new(CountingToolLlm {
            calls: calls.clone(),
            input_tokens_per_call: 1,
        });
        let (notif_tx, _rx) = tokio::sync::mpsc::channel(64);
        let runner = crate::supervisor_runner::build_runner(
            client,
            None,
            Arc::new(RuntimeSkills::build(vec![])),
            SkillsConfig::default(),
            None,
            None,
            None,
            Some(empty_pending_approvals()),
            Some(notif_tx),
            1,
            vec![],
            vec![],
            Some(3),
            None,
        );
        let _ = runner.run_sync(loop_spec("loop")).await;
        let n = calls.load(Ordering::Relaxed);
        // 3 loop iterations; the graceful summary turn (step 4) adds at most one
        // more. Before wiring, the default cap (25) lets it run far past this.
        assert!(
            n <= 4,
            "expected the loop capped near 3 iterations, got {n} generate() calls"
        );
        assert!(n >= 3, "expected at least 3 iterations, got {n}");
    }

    /// A tool whose output CHANGES on every call even when the args are
    /// identical — models e.g. `cargo build` returning new diagnostics after
    /// each intervening edit. Used to prove the doom-loop guard keys on the
    /// (tool, args, result) triple, not (tool, args) alone.
    struct VaryingResultTool {
        calls: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for VaryingResultTool {
        fn name(&self) -> &str {
            "build"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "build".into(),
                description: "test build tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<String, crate::tools::ToolError> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            // Distinct content each call -> distinct result fingerprint.
            Ok(format!("build output #{n}"))
        }
    }

    /// A tool whose output is CONSTANT on every call (same args -> same
    /// result), modelling a genuinely stuck retry. Trips the doom-loop guard.
    struct ConstantResultTool;

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for ConstantResultTool {
        fn name(&self) -> &str {
            "build"
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: "build".into(),
                description: "test build tool".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<String, crate::tools::ToolError> {
            Ok("identical build output".into())
        }
    }

    fn build_tool_call_response(call_id: &str) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: call_id.into(),
                tool_name: "build".into(),
                // IDENTICAL args every turn: only the result varies.
                input: serde_json::json!({}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        }
    }

    /// Fix #1a — progress is NOT a loop: the SAME tool call (identical args)
    /// every turn, but whose execution returns a DIFFERENT result each time,
    /// must NOT trip the doom-loop guard. Under the old (tool, args)-only
    /// fingerprint this aborts with "loop_detected"; under the (tool, args,
    /// result) fingerprint it runs to the iteration cap instead.
    #[tokio::test]
    async fn varying_tool_results_are_not_a_doom_loop() {
        use crate::llm::stub::SequenceLlm;
        // Always emits the same call; the loop only ever stops on a budget.
        let responses: Vec<crate::llm::LlmResponse> = vec![
            build_tool_call_response("c-0"),
            build_tool_call_response("c-1"),
            build_tool_call_response("c-2"),
            build_tool_call_response("c-3"),
            build_tool_call_response("c-4"),
            build_tool_call_response("c-5"),
            end_turn_response("ITER SUMMARY: capped by iteration budget."),
        ];
        let calls = Arc::new(AtomicU64::new(0));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(VaryingResultTool {
                    calls: calls.clone(),
                })])
                .with_tools_policy(vec![mur_common::agent::ToolRule {
                    pattern: "build".into(),
                    policy: mur_common::agent::ToolPolicy::Allow,
                    risk: None,
                }])
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                // Small cap so the test terminates fast; doom-loop must NOT
                // fire before the cap is reached.
                .with_max_iterations(5),
        );
        let outcome = runner.run_sync(loop_spec("progress")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        let usage = task.usage.expect("budget exit must populate usage");
        assert_ne!(
            usage["stop_reason"], "loop_detected",
            "changing results must NOT be a doom loop; usage={usage}"
        );
        assert_eq!(
            usage["stop_reason"], "max_iterations",
            "expected the iteration cap to be the terminus; usage={usage}"
        );
    }

    /// Fix #1b — genuine stuck IS a loop: the SAME tool call AND identical
    /// result each turn still aborts with stop_reason "loop_detected" within
    /// ~3 iterations, well below the iteration cap.
    #[tokio::test]
    async fn identical_tool_results_still_trip_doom_loop() {
        use crate::llm::stub::SequenceLlm;
        let responses: Vec<crate::llm::LlmResponse> = vec![
            build_tool_call_response("s-0"),
            build_tool_call_response("s-1"),
            build_tool_call_response("s-2"),
            end_turn_response("LOOP SUMMARY: stuck; identical output."),
        ];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(ConstantResultTool)])
                .with_tools_policy(vec![mur_common::agent::ToolRule {
                    pattern: "build".into(),
                    policy: mur_common::agent::ToolPolicy::Allow,
                    risk: None,
                }])
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(50),
        );
        let outcome = runner.run_sync(loop_spec("stuck")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (doom-loop graceful exit), got {outcome:?}");
        };
        let usage = task.usage.expect("doom-loop exit must populate usage");
        assert_eq!(usage["stop_reason"], "loop_detected", "usage={usage}");
        let iters = usage["iterations"]
            .as_u64()
            .expect("iterations is a number");
        assert!(iters < 5, "expected early abort, got {iters} iterations");
    }

    /// Fix #2 — graceful_exit must sanitize a dangling tool_use before the
    /// final summary turn. We build a `history` ending in a `ToolUse` whose
    /// call has NO following `ToolResults` (mid-iteration abort), then run
    /// `graceful_exit`. A recording LLM captures the request it receives; we
    /// assert every tool_use call_id in that request has a matching
    /// tool_result, so the Anthropic API would not 400 on it.
    #[tokio::test]
    async fn graceful_exit_sanitizes_dangling_tool_use() {
        use crate::llm::{RichMessage, ToolCallResult};

        /// Captures the messages of the request passed to it and replies with
        /// a benign end-turn summary.
        struct RecordingLlm {
            seen: Arc<Mutex<Vec<RichMessage>>>,
        }
        #[async_trait::async_trait]
        impl crate::llm::LlmClient for RecordingLlm {
            async fn generate(
                &self,
                req: crate::llm::LlmRequest,
            ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
                *self.seen.lock().unwrap() = req.messages.clone();
                Ok(end_turn_response("SUMMARY: done."))
            }
            fn model_name(&self) -> &str {
                "recording"
            }
        }

        let seen = Arc::new(Mutex::new(Vec::new()));
        let client: Arc<dyn crate::llm::LlmClient> = Arc::new(RecordingLlm { seen: seen.clone() });
        let runner = TaskRunner::with_llm(client.clone());

        // History ends with a tool_use that has no following tool_result.
        let history = vec![
            RichMessage::Text {
                role: "system".into(),
                content: "sys".into(),
            },
            RichMessage::Text {
                role: "user".into(),
                content: "do the thing".into(),
            },
            RichMessage::ToolUse {
                text: Some("calling build".into()),
                calls: vec![ToolCallResult {
                    call_id: "dangling-1".into(),
                    tool_name: "build".into(),
                    input: serde_json::json!({}),
                }],
            },
        ];

        let msg = runner
            .graceful_exit(client.as_ref(), &history, LoopStop::LoopDetected)
            .await;
        // Summary turn succeeded (not the fallback path).
        assert_eq!(text_of(&msg), "SUMMARY: done.");

        // Inspect what the LLM actually received: collect every tool_use id and
        // every tool_result id, then assert no tool_use id is unmatched.
        let messages = seen.lock().unwrap().clone();
        let mut use_ids: Vec<String> = Vec::new();
        let mut result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for m in &messages {
            match m {
                RichMessage::ToolUse { calls, .. } => {
                    for c in calls {
                        use_ids.push(c.call_id.clone());
                    }
                }
                RichMessage::ToolResults { results } => {
                    for r in results {
                        result_ids.insert(r.call_id.clone());
                    }
                }
                RichMessage::Text { .. } => {}
            }
        }
        assert!(!use_ids.is_empty(), "expected at least one tool_use");
        for id in &use_ids {
            assert!(
                result_ids.contains(id),
                "tool_use id {id} has no matching tool_result; request would 400. \
                 result_ids={result_ids:?}"
            );
        }
    }
}
