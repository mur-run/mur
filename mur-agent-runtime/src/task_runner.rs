//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx, PromptView, ToolCall, ToolResult};
use crate::llm::{LlmClient, LlmError, LlmRequest, RequestIntent};
use crate::skills::RuntimeSkills;
use crate::skills::injector::inject_layer2;
use crate::skills::trigger_matcher::{format_layer3, layer3_body, match_prompt};
use crate::telemetry_writer::{Event, SkillOutcome};
use mur_common::a2a::{Message, MessagePart, Task, TaskError, TaskState};
use mur_common::config::SkillsConfig;
use mur_common::skill::McpInventory;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
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
    /// Active fleet name for this turn, derived from a `fleet-<name>` channel id
    /// by the `channel/delegate` handler. Drives fleet-scoped skill injection;
    /// `None` for non-fleet turns, so fleet-scoped skills stay hidden outside
    /// their fleet (fail-closed).
    pub active_fleet: Option<String>,
    /// Active team id for this turn, derived from the fleet's `team_id` field
    /// by the `channel/delegate` handler. Drives team-scoped skill injection;
    /// `None` for non-fleet turns or fleets without a team (fail-closed).
    pub active_team: Option<String>,
    /// Why this turn is being run (see `RequestIntent`). Deliberately NOT
    /// `Default`-derived on `TaskSpec` — every construction site must state
    /// its intent explicitly so an interactive (user-facing) call site can
    /// never silently fall through to `Background` (which would make it
    /// eligible for Smart cheap-model routing). Runtime-initiated call
    /// sites (cron scheduler, idle scheduler, watch scheduler) tag
    /// `Background`; chat / A2A `message/send` / `channel/delegate` tag
    /// `Interactive`.
    pub intent: RequestIntent,
    /// When set, the LLM is instructed to write its complete output to this
    /// file path and return only the path in its reply. After the task
    /// completes, the runtime verifies the file exists, computes its hash,
    /// populates `Task.artifacts`, and replaces the assistant reply with a
    /// short `[File: path]` reference. Callers then read the file
    /// byte-by-byte instead of re-typing content through another LLM
    /// (issue #715 Part B).
    pub output_artifact_path: Option<std::path::PathBuf>,
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

/// Cap on chat threads retained for multi-turn memory (LRU-evicted) so a
/// long-lived agent serving many conversations doesn't leak unboundedly.
const MAX_CONVERSATIONS: usize = 256;
/// Cap on stored messages per conversation (user+assistant turns; the system
/// prompt is re-prepended fresh each turn, not stored). MUST stay even: stored
/// history is always user/assistant pairs, and trimming an even count keeps the
/// first message a `user` turn (Anthropic requires that). ponytail: crude count
/// cap — switch to a token budget if turn sizes vary wildly.
const MAX_CONV_MESSAGES: usize = 40;

/// Injected into every agent's system prompt so authored files land where MUR
/// can read them instead of the working directory. Guidance, not enforcement.
const OUTPUT_LOCATIONS_RULE: &str = "\n\n## Output locations\n\
When you produce files, put them where MUR can read them — never write them into the current working directory (often a source tree):\n\
- Knowledge objects (workflows, skills, notes): register with the real command so they land in ~/.mur and show up in MUR and the Hub — `mur skill install <path>` for a skill, `mur workflow new` for a workflow. Never leave the definition in the working directory.\n\
- Run artifacts (reports, quarantined files, scratch output): write to ~/.mur/artifacts/<your-agent-name>/<run>/, where <run> is a short timestamp or task label. Never the working directory.\n\
- The only reason to write into the working directory is to edit an existing file in a repository you have been granted access to.";

/// Injected into the system prompt when `TaskSpec.output_artifact_path` is
/// set. Tells the agent to write its full output to the designated file and
/// return only the path — the runtime then verifies the file and replaces the
/// reply with a short artifact reference, so callers never re-type content
/// through another LLM (issue #715 Part B).
const ARTIFACT_RULE: &str = "\n\n## Artifact output path\n\
Your complete final output for this turn must be written to `{path}` using write_file.\n\
In your reply, state ONLY the file path and a one-line summary of what was written.\n\
Do NOT include the file content in your reply — the caller will read the file directly.";

/// In-memory multi-turn chat memory. The CLI and Hub thread `context.task_id` =
/// the prior reply's id on every send, so we key stored history by the id of the
/// turn that produced it; the next turn's `context.task_id` then recalls its
/// predecessor. Stores text only — a pasted image was seen the turn it arrived
/// and is not re-sent on later turns. ponytail: resets on runtime restart and is
/// NOT a `--resume` persistence store; back it with disk if cross-restart model
/// memory is ever needed.
#[derive(Default)]
struct ConversationStore {
    map: HashMap<String, Vec<crate::llm::RichMessage>>,
    /// Insertion order for LRU eviction past `MAX_CONVERSATIONS`.
    order: VecDeque<String>,
}

impl ConversationStore {
    /// Prior conversation for `key` (the caller's `context.task_id`), or empty.
    fn prior(&self, key: Option<&str>) -> Vec<crate::llm::RichMessage> {
        key.and_then(|k| self.map.get(k))
            .cloned()
            .unwrap_or_default()
    }

    /// Store `history` (user/assistant pairs) under `key`, trimming oldest turns
    /// and evicting the oldest conversation if over the caps.
    fn remember(&mut self, key: String, mut history: Vec<crate::llm::RichMessage>) {
        if history.len() > MAX_CONV_MESSAGES {
            history.drain(0..history.len() - MAX_CONV_MESSAGES);
        }
        if self.map.insert(key.clone(), history).is_none() {
            self.order.push_back(key);
            while self.order.len() > MAX_CONVERSATIONS {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

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
    /// Parallel to `cumulative_input_tokens` (runner-lifetime, shared across
    /// tasks). Not used by the loop's own input-only token budget; it exists so
    /// `run_sync_inner` can snapshot-delta both counters and report a turn's
    /// real input+output token usage in `Task.usage` (fleet cost accounting).
    cumulative_output_tokens: AtomicU64,
    /// Input-token count of the most recent single LLM call. Approximates the
    /// current context window fill, unlike the cumulative total. Read by the
    /// `token_usage` closure to populate `context_tokens` in per-turn usage JSON
    /// so the CLI glass-box bar can show a live context gauge.
    last_input_tokens: AtomicU64,
    /// `model_ref` of the most recent successful LLM response, read by the
    /// `token_usage` closure to populate `Task.usage.model_ref`. Reset to
    /// `None` at the start of each `run_sync_inner` turn so a stub/misconfigured
    /// backend (which never calls a real model) reports no model rather than a
    /// stale one from a previous turn. Same runner-lifetime concurrency caveat
    /// as `cumulative_input_tokens`: overlapping turns on one runner can race
    /// this value; acceptable for a best-effort telemetry field.
    last_model_ref: Mutex<Option<String>>,
    /// True when the most recent LLM response of the current turn was cut off
    /// at the provider's max_tokens ceiling (`StopReason::MaxTokens`). Read by
    /// the `token_usage` closure to add `"truncated": true` to `Task.usage`;
    /// reset alongside `last_model_ref` at the start of each `run_sync_inner`
    /// turn. Same runner-lifetime concurrency caveat as `last_model_ref`.
    last_turn_truncated: AtomicBool,
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
    /// Per-turn steering channels keyed by task id. A running agentic loop
    /// holds the receiver; `turn/steer` pushes a user interjection here and the
    /// loop picks it up at the next iteration boundary.
    steering: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::mpsc::Sender<String>>>>,
    hitl_timeout_secs: u32,
    max_iterations: u32,
    /// Per-task ceiling on cumulative input tokens for the agentic loop. The
    /// loop snapshots the (per-runner) counter at entry and stops gracefully
    /// once `current - start >= max_token_budget`.
    max_token_budget: u64,
    tools: Vec<Arc<dyn crate::tools::ToolExecutor>>,
    tools_policy: Vec<mur_common::agent::ToolRule>,
    /// Per-agent effort for this agent's own turns. `None` = the API default.
    /// Mechanical internal calls override it downward regardless (see
    /// `graceful_exit`).
    effort: Option<mur_common::llm::Effort>,
    /// Multi-turn chat memory keyed by `context.task_id` (see `ConversationStore`).
    conversations: Mutex<ConversationStore>,
    /// Set by `begin_drain()` during graceful shutdown. When true, `run_sync_inner`
    /// rejects new turns immediately with a transient failure so in-flight work
    /// can finish before transports are torn down.
    draining: Arc<AtomicBool>,
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

/// Max number of times a single turn retries an `LlmError::RateLimit` (HTTP
/// 429) before giving up and propagating the error. Separate from the
/// empty-stream retry's own attempt counter.
const MAX_RATE_LIMIT_RETRIES: u8 = 3;

/// Base delay for the rate-limit backoff: attempt N sleeps
/// `RATE_LIMIT_BACKOFF_BASE * 2^N` (1-indexed attempts give 2s, 4s, 8s, ...).
const RATE_LIMIT_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(1);

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
            cumulative_output_tokens: AtomicU64::new(0),
            last_input_tokens: AtomicU64::new(0),
            last_model_ref: Mutex::new(None),
            last_turn_truncated: AtomicBool::new(false),
            hook_chain: None,
            hook_ctx: None,
            hook_cancel: None,
            pending_approvals: None,
            notifier: None,
            client_notifiers: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            steering: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            hitl_timeout_secs: 300,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            max_token_budget: DEFAULT_MAX_TOKEN_BUDGET,
            tools: vec![],
            tools_policy: vec![],
            effort: None,
            conversations: Mutex::new(ConversationStore::default()),
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

    /// Build this turn's LLM message list: system prompt, then the prior
    /// conversation threaded via `ctx` (the caller's `context.task_id`), then the
    /// current user message. With no `ctx`/no stored history this is just
    /// `[system, user]` — identical to the old stateless behavior.
    fn seed_history(
        &self,
        ctx: Option<&str>,
        system: String,
        input: &Message,
    ) -> Vec<crate::llm::RichMessage> {
        let mut h = Vec::new();
        if !system.is_empty() {
            h.push(crate::llm::RichMessage::Text {
                role: "system".into(),
                content: system,
            });
        }
        h.extend(
            self.conversations
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .prior(ctx),
        );
        h.push(user_message(input));
        h
    }

    /// Persist this turn into multi-turn memory keyed by `key` (this turn's id),
    /// so the next send — whose `context.task_id` equals `key` — recalls it.
    /// Stores text only (this turn's tool scaffolding and any pasted image stay
    /// ephemeral). Roles `user`/`agent` map to Anthropic `user`/`assistant`.
    fn remember_turn(&self, key: &str, ctx: Option<&str>, input: &Message, reply: &Message) {
        let mut store = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = store.prior(ctx);
        h.push(crate::llm::RichMessage::Text {
            role: "user".into(),
            content: text_of(input),
        });
        h.push(crate::llm::RichMessage::Text {
            role: "agent".into(),
            content: text_of(reply),
        });
        store.remember(key.to_string(), h);
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn crate::tools::ToolExecutor>>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_tools_policy(mut self, rules: Vec<mur_common::agent::ToolRule>) -> Self {
        self.tools_policy = rules;
        self
    }

    /// Set the agent's per-turn effort (from its profile).
    pub fn with_effort(mut self, effort: Option<mur_common::llm::Effort>) -> Self {
        self.effort = effort;
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

    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Set the per-task cumulative input-token budget for the agentic loop.
    pub fn with_max_token_budget(mut self, n: u64) -> Self {
        self.max_token_budget = n;
        self
    }

    fn assemble_system_prompt(
        &self,
        user_prompt: &str,
        active_fleet: Option<&str>,
        active_team: Option<&str>,
    ) -> (String, Vec<String>) {
        let mut base = self.system_prompt.clone().unwrap_or_default();
        base.push_str(OUTPUT_LOCATIONS_RULE);
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
        // Scope filter: project from the member's cwd repo root (shared detection
        // with the CLI hook); fleet from the turn's `fleet-<name>` channel id,
        // threaded in by the `channel/delegate` handler. Fleet- and project-scoped
        // skills only surface in their matching context; user/enterprise always.
        let active_project = mur_common::project::active_project_id();
        let injection = inject_layer2(
            &skills.loaded,
            &self.skills_cfg,
            ctx_fill,
            &recently,
            active_fleet,
            active_project.as_deref(),
            active_team,
        );

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
            let Some(mut body) = layer3_body(&loaded.manifest, &inventory) else {
                continue;
            };
            if let Some(hint) = crate::skills::trigger_matcher::bundle_hint(&loaded.dir) {
                body.push_str(&hint);
            }
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

    async fn run_sync_inner(
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
                        let mut system = self
                            .prepare_system_prompt(
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
                        self.run_agentic_loop(
                            &id,
                            client.as_ref(),
                            system,
                            &spec.input,
                            spec.context_task_id.as_deref(),
                            sink,
                            steer_rx,
                            spec.intent,
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

    // ponytail: one private method, one extra threaded id — an args struct would
    // be ceremony for no caller benefit.
    #[allow(clippy::too_many_arguments)]
    async fn run_llm(
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

        let (mut system, fired) = self.assemble_system_prompt(&prompt, active_fleet, active_team);
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
        let messages = self.seed_history(context_task_id, system, input);
        let req = LlmRequest {
            messages,
            temperature: None,
            max_tokens: None,
            tools: vec![],
            effort: self.effort,
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

    /// Handle a final answer the provider cut off at the max_tokens ceiling:
    /// warn loudly, flag the turn for `Task.usage` (`"truncated": true`), and
    /// append the visible truncation marker so every downstream consumer —
    /// user, delegating agent, channel history — can see the cut instead of a
    /// silent mid-word seam (#715). The effective ceiling is the provider
    /// default (requests leave `max_tokens` unset), so the warning reports the
    /// actual `output_tokens`, which equals the cap at truncation.
    fn mark_max_tokens_truncation(&self, task_id: &str, resp: &mut crate::llm::LlmResponse) {
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

    async fn prepare_system_prompt(
        &self,
        input: &Message,
        active_fleet: Option<&str>,
        active_team: Option<&str>,
    ) -> Result<String, TaskError> {
        let prompt = text_of(input);
        let (system, _fired) = self.assemble_system_prompt(&prompt, active_fleet, active_team);
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

        // Resolve step notifier once (route by task id, fall back to baked notifier).
        // Used for step/started + step/completed in both Allow and Ask arms.
        let step_notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>> = {
            let routed = self.client_notifiers.lock().await.get(task_id).cloned();
            routed.or_else(|| self.notifier.clone())
        };
        let step_id = uuid::Uuid::now_v7().to_string();

        // 1b. Policy gate: check before executing.
        {
            use mur_common::agent::{ToolPolicy, resolve_tool_policy_opt};
            let policy = if crate::tools::suggest::suggest_replies_allowed(&call.tool_name) {
                ToolPolicy::Allow
            } else {
                // issue #3: dispatch/spend tools (parallel_jobs, fleet_run,
                // delegate_to) must ask BEFORE executing. The HITL gate below
                // is now a pre-execution gate (execute happens only after an
                // Allow decision), so `Ask` provides real spend protection and
                // fleet_run no longer needs its old `None => Allow` special
                // case. Unknown tools fall through to `ToolPolicy::default()`,
                // which is `Ask` — fail-closed.
                resolve_tool_policy_opt(&self.tools_policy, &call.tool_name).unwrap_or_default()
            };
            match policy {
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
                    if let Some(ref n) = step_notifier {
                        let _ = n
                            .send(step_notification(
                                "step/started",
                                serde_json::json!({
                                    "step_id": step_id,
                                    "task_id": task_id,
                                    "kind": "tool",
                                    "name": call.tool_name,
                                    "args": call.input,
                                }),
                            ))
                            .await;
                    }
                    let t0 = std::time::Instant::now();
                    let (output, is_error) = match tool.execute(call.input.clone()).await {
                        Ok(out) => (out, false),
                        Err(e) => (format!("tool error: {e}"), true),
                    };
                    if let Some(ref n) = step_notifier {
                        let (out, truncated, full_len) = cap_step_output(&output);
                        let _ = n
                            .send(step_notification(
                                "step/completed",
                                serde_json::json!({
                                    "step_id": step_id,
                                    "task_id": task_id,
                                    "ok": !is_error,
                                    "output": out,
                                    "truncated": truncated,
                                    "full_len": full_len,
                                    "error": if is_error {
                                        serde_json::Value::String(output.clone())
                                    } else {
                                        serde_json::Value::Null
                                    },
                                    "duration_ms": t0.elapsed().as_millis() as u64,
                                }),
                            ))
                            .await;
                    }
                    return Ok(ToolResultEntry {
                        call_id: call.call_id.clone(),
                        content: output,
                        is_error,
                    });
                }
                ToolPolicy::Ask => {
                    // issue #3: PRE-EXECUTION approval gate. The tool is NOT
                    // executed until an Allow decision arrives. Route the
                    // prompt to the connection that issued this turn; never
                    // broadcast. fail-closed: with no approval sink wired
                    // (pending_approvals or notifier missing) the decision is
                    // DENY, so dispatch/spend tools cannot run unattended.
                    let routed = self.client_notifiers.lock().await.get(task_id).cloned();
                    let effective_notifier = routed.as_ref().or(self.notifier.as_ref());
                    let decision = if let (Some(pa), Some(notifier)) =
                        (&self.pending_approvals, effective_notifier)
                    {
                        let hitl_id = uuid::Uuid::now_v7().to_string();
                        let (tx, rx) = tokio::sync::oneshot::channel::<crate::hitl::HitlDecision>();
                        pa.lock().await.insert(hitl_id.clone(), tx);
                        let notification = serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "tool/approval_needed",
                            "params": {
                                "step_id": step_id,
                                "hitl_id": hitl_id,
                                "task_id": task_id,
                                "tool_name": call.tool_name,
                                "tool_input": call.input,
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
                        // fail-closed: no approval sink => deny.
                        crate::hitl::HitlDecision {
                            allow: false,
                            reason: Some("no approval channel available".into()),
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
                    // Approved: fall through to execute below.
                }
            }
        }

        // 2. Execute the tool
        let tool = tool.unwrap();
        if let Some(ref n) = step_notifier {
            let _ = n
                .send(step_notification(
                    "step/started",
                    serde_json::json!({
                        "step_id": step_id,
                        "task_id": task_id,
                        "kind": "tool",
                        "name": call.tool_name,
                        "args": call.input,
                    }),
                ))
                .await;
        }
        let t0_ask = std::time::Instant::now();
        let (output, is_error) = match tool.execute(call.input.clone()).await {
            Ok(out) => (out, false),
            Err(e) => (format!("tool error: {e}"), true),
        };
        if let Some(ref n) = step_notifier {
            let (out, truncated, full_len) = cap_step_output(&output);
            let _ = n
                .send(step_notification(
                    "step/completed",
                    serde_json::json!({
                        "step_id": step_id,
                        "task_id": task_id,
                        "ok": !is_error,
                        "output": out,
                        "truncated": truncated,
                        "full_len": full_len,
                        "error": if is_error {
                            serde_json::Value::String(output.clone())
                        } else {
                            serde_json::Value::Null
                        },
                        "duration_ms": t0_ask.elapsed().as_millis() as u64,
                    }),
                ))
                .await;
        }

        // The HITL approval gate now runs PRE-execution in the `Ask` policy
        // arm above (issue #3), so by the time we reach here the tool has been
        // approved and executed. Return its output.
        Ok(ToolResultEntry {
            call_id: call.call_id.clone(),
            content: output,
            is_error,
        })
    }

    /// Fold the hook chain's `post_tool_use` patch into each tool result,
    /// rewriting `content` when a hook returns `replace_output` — e.g.
    /// `CompressHook` offloading an oversized output (size-gated by
    /// `compress.yaml` `auto.min_tokens`) or B0 rule 8 PII redaction.
    /// Mutates in place; no-op when no chain is wired or every hook returns
    /// `None`. Closes the M7.6 gap where the patch was computed but discarded.
    async fn apply_post_tool_use(
        &self,
        calls: &[crate::llm::ToolCallResult],
        results: &mut [crate::llm::ToolResultEntry],
    ) {
        let (Some(chain), Some(ctx), Some(cancel)) =
            (&self.hook_chain, &self.hook_ctx, &self.hook_cancel)
        else {
            return;
        };
        let mut turn_ctx = ctx.clone();
        turn_ctx.turn_id = self.turn_counter.load(Ordering::Relaxed);
        for (call, entry) in calls.iter().zip(results.iter_mut()) {
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
                duration_ms: 0,
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

    /// Run the agentic loop. Returns the final agent message plus an optional
    /// `LoopStop` describing which budget (if any) forced an early, graceful
    /// exit. `None` means the model ended the turn naturally.
    #[allow(clippy::too_many_arguments)]
    async fn run_agentic_loop(
        &self,
        task_id: &str,
        client: &dyn crate::llm::LlmClient,
        system_prompt: String,
        input: &Message,
        context_task_id: Option<&str>,
        sink: Option<tokio::sync::mpsc::Sender<crate::llm::StreamDelta>>,
        mut steer_rx: Option<tokio::sync::mpsc::Receiver<String>>,
        intent: RequestIntent,
    ) -> Result<(Message, Option<LoopExit>), TaskError> {
        use crate::llm::{LlmRequest, RichMessage, StopReason};

        // `suggest_replies` is offered to the model only on streaming
        // (interactive) turns — non-interactive callers never see it.
        let streaming = sink.is_some();
        let tool_defs: Vec<_> = self
            .tools_for_loop()
            .iter()
            .map(|t| t.def())
            .filter(|d| crate::tools::suggest::offer_for_streaming(&d.name, streaming))
            .collect();
        // Seed with prior conversation threaded via `context.task_id` so the
        // model has multi-turn memory; this turn's tool scaffolding is appended
        // below and stays ephemeral (never persisted into chat memory).
        let mut history: Vec<RichMessage> =
            self.seed_history(context_task_id, system_prompt, input);

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
                effort: self.effort,
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
                        Some(s) => client.generate_stream(req_try, s.clone()).await,
                        None => client.generate(req_try).await,
                    };
                    match result {
                        Ok(r) => break r,
                        Err(LlmError::InvalidResponse(ref msg))
                            if attempt == 0 && msg.contains("empty streamed response") =>
                        {
                            attempt += 1;
                            continue;
                        }
                        // Transient 429: back off exponentially and retry, up to
                        // MAX_RATE_LIMIT_RETRIES times, so a momentary burst
                        // across parallel agents doesn't kill the turn outright.
                        Err(LlmError::RateLimit) if rate_limit_attempt < MAX_RATE_LIMIT_RETRIES => {
                            rate_limit_attempt += 1;
                            let delay = rate_limit_backoff_delay(rate_limit_attempt);
                            tracing::warn!(
                                attempt = rate_limit_attempt,
                                delay_secs = delay.as_secs(),
                                "llm rate limited (429); backing off and retrying"
                            );
                            tokio::time::sleep(delay).await;
                            continue;
                        }
                        Err(e) => {
                            return Err(task_error("llm_error", format!("{e}"), true));
                        }
                    }
                }
            };

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

            // Fold post-tool-use hook patches into the results before
            // fingerprinting / history so the model sees the rewritten text:
            // CompressHook offloads oversized output (size-gated) and B0 rule 8
            // redacts PII. Patches are content-deterministic, so the doom-loop
            // fingerprint below stays stable.
            self.apply_post_tool_use(&resp.tool_calls, &mut results)
                .await;

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
            // Mechanical: recap what happened, no tools, no decisions — and it
            // fires exactly when a budget or loop guard already tripped, so
            // inheriting the agent's `xhigh` for a post-mortem is the wrong
            // direction. Pinned low regardless of the profile.
            effort: Some(mur_common::llm::Effort::Low),
            tools: vec![], // tools disabled: force a textual summary
            ..Default::default()
        };
        let mut text = match client.generate(req).await {
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
                    .unwrap_or_else(|| format!("Stopped: {} budget reached.", reason.as_str()))
            }
        };
        // #595: mark output that ended at the iteration cap so partial
        // execution is visible instead of silently reported as clean.
        if matches!(reason, LoopStop::MaxIterations) {
            text.push_str(
                "\n\n[runtime: turn ended at the iteration cap — output may be incomplete]",
            );
        }
        Message {
            role: "agent".into(),
            parts: vec![mur_common::a2a::MessagePart::Text { text }],
        }
    }
}

/// Backoff delay for rate-limit retry attempt `attempt` (1-indexed: the first
/// retry is attempt 1). Doubles `RATE_LIMIT_BACKOFF_BASE` per attempt, giving
/// 2s, 4s, 8s for attempts 1, 2, 3 with the current base of 1s.
fn rate_limit_backoff_delay(attempt: u8) -> std::time::Duration {
    RATE_LIMIT_BACKOFF_BASE * (1u32 << u32::from(attempt))
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
/// Read `path` from disk, return `ArtifactInfo` with SHA-256 hash and size
/// when the file exists and is readable. `None` on any error (absent/missing
/// permissions/empty) — the task falls back to the inline reply without
/// swallowing errors (callers still get the full LLM text).
fn detect_artifact(path: &std::path::Path) -> Option<Vec<mur_common::a2a::ArtifactInfo>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(meta.len().min(256 * 1024) as usize);
    file.read_to_end(&mut buf).ok()?;
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(&buf));
    Some(vec![mur_common::a2a::ArtifactInfo {
        path: path.to_string_lossy().into_owned(),
        mime_type: guess_mime_type(path).unwrap_or_else(|| "application/octet-stream".to_string()),
        sha256: Some(hash),
        size_bytes: meta.len(),
    }])
}

/// Simple extension-based MIME guess. Not exhaustive — the artifact metadata
/// is advisory and callers who need precise MIME should probe the content.
fn guess_mime_type(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "md" | "markdown" => "text/markdown",
            "txt" => "text/plain",
            "json" => "application/json",
            "yaml" | "yml" => "application/x-yaml",
            "html" | "htm" => "text/html",
            "csv" => "text/csv",
            "toml" => "application/toml",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "svg" => "image/svg+xml",
            "pdf" => "application/pdf",
            "rs" => "text/x-rust",
            "ts" | "tsx" => "text/typescript",
            _ => "application/octet-stream",
        }
        .into(),
    )
}

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

/// Build the user-turn message: image+text when `input` carries a pasted
/// image (a screenshot from `mur agent cli`), else plain text. Images skip the
/// B0 text hook (they're binary, not prompt-injectable). ponytail: OCR-scan
/// inbound images later if needed.
fn user_message(input: &Message) -> crate::llm::RichMessage {
    use crate::llm::RichMessage;
    let text = text_of(input);
    let image = input.parts.iter().find_map(|p| match p {
        MessagePart::Data { mime_type, data } if mime_type.starts_with("image/") => data
            .get("base64")
            .and_then(|v| v.as_str())
            .map(|b64| (mime_type.clone(), b64.to_string())),
        _ => None,
    });
    match image {
        Some((media_type, data)) => RichMessage::ImageText {
            role: input.role.clone(),
            media_type,
            data,
            text,
        },
        None => RichMessage::Text {
            role: input.role.clone(),
            content: text,
        },
    }
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
                artifacts: None,
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

/// Maximum byte size of tool output included inline in a `step/completed`
/// notification. Larger outputs are truncated; full recovery in a later phase.
pub(crate) const STEP_MAX_BYTES: usize = 8 * 1024;

/// Wrap params in a JSON-RPC notification envelope — mirrors the existing
/// `tool/approval_needed` shape used on the streaming socket.
pub(crate) fn step_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// Cap tool output to `STEP_MAX_BYTES` on a char boundary.
/// Returns `(capped_output, was_truncated, full_byte_len)`.
pub(crate) fn cap_step_output(output: &str) -> (String, bool, usize) {
    let full_len = output.len();
    if full_len <= STEP_MAX_BYTES {
        return (output.to_string(), false, full_len);
    }
    let mut cut = STEP_MAX_BYTES;
    while !output.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut s = output[..cut].to_string();
    s.push_str("\n[truncated]");
    (s, true, full_len)
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

    /// Stub hook: replaces any tool output longer than 10 chars with
    /// "OFFLOADED" (stands in for CompressHook's size-gated offload). Proves
    /// the turn loop consumes `replace_output` rather than discarding it.
    struct ReplaceBigHook;
    #[async_trait::async_trait]
    impl crate::hooks::Hook for ReplaceBigHook {
        fn name(&self) -> &str {
            "ReplaceBigHook"
        }
        async fn post_tool_use(
            &self,
            _ctx: &HookCtx,
            _call: &ToolCall,
            result: &ToolResult,
            _tok: &CancellationToken,
        ) -> Result<crate::hooks::PostToolUsePatch, crate::hooks::HookError> {
            let big = result
                .output
                .as_str()
                .map(|s| s.len() > 10)
                .unwrap_or(false);
            Ok(crate::hooks::PostToolUsePatch {
                replace_output: big.then(|| serde_json::Value::String("OFFLOADED".into())),
            })
        }
    }

    #[tokio::test]
    async fn apply_post_tool_use_rewrites_oversized_output() {
        use crate::llm::{ToolCallResult, ToolResultEntry};
        let chain = Arc::new(HookChain::new(vec![Arc::new(ReplaceBigHook)]));
        let runner = TaskRunner::new_stub_echo().with_hook_chain(
            chain,
            HookCtx::for_test_with_home(std::path::PathBuf::from("."), 0),
            CancellationToken::new(),
        );
        let calls = vec![
            ToolCallResult {
                call_id: "c1".into(),
                tool_name: "big".into(),
                input: serde_json::json!({}),
            },
            ToolCallResult {
                call_id: "c2".into(),
                tool_name: "small".into(),
                input: serde_json::json!({}),
            },
        ];
        let mut results = vec![
            ToolResultEntry {
                call_id: "c1".into(),
                content: "this is a large tool output".into(),
                is_error: false,
            },
            ToolResultEntry {
                call_id: "c2".into(),
                content: "ok".into(),
                is_error: false,
            },
        ];
        runner.apply_post_tool_use(&calls, &mut results).await;
        assert_eq!(
            results[0].content, "OFFLOADED",
            "oversized output rewritten"
        );
        assert_eq!(results[1].content, "ok", "small output untouched");
    }

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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
        };
        let outcome = runner.run_sync(spec).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed")
        };
        assert_eq!(task.id, "task-supplied-9");
    }

    fn user_turn(text: &str, task_id: &str, ctx: Option<&str>) -> TaskSpec {
        TaskSpec {
            input: mur_common::a2a::Message {
                role: "user".into(),
                parts: vec![MessagePart::Text { text: text.into() }],
            },
            context_task_id: ctx.map(str::to_string),
            task_id: Some(task_id.to_string()),
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
        }
    }

    #[tokio::test]
    async fn threads_multi_turn_chat_memory() {
        let runner = TaskRunner::new_stub_echo();
        // Turn 1 — no prior context.
        let _ = runner.run_sync(user_turn("first", "t1", None)).await;
        // Turn 2 — threads context.task_id = t1 (the prior reply's id), exactly
        // as the CLI/Hub clients do.
        let _ = runner.run_sync(user_turn("second", "t2", Some("t1"))).await;

        let store = runner.conversations.lock().unwrap();
        // t1 holds just its own pair; t2 accumulated the prior turn + this one.
        assert_eq!(store.map.get("t1").map(|h| h.len()), Some(2));
        let t2 = store.map.get("t2").expect("turn 2 remembered");
        assert_eq!(t2.len(), 4, "2 prior + 2 current = 2 user + 2 agent");
        // Turn 1's user message survives into turn 2's memory (the bug was that
        // it didn't — every turn started from an empty history).
        match &t2[0] {
            crate::llm::RichMessage::Text { role, content } => {
                assert_eq!(role, "user");
                assert_eq!(content, "first");
            }
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn seed_history_prepends_prior_conversation() {
        use crate::llm::RichMessage;
        let runner = TaskRunner::new_stub_echo();
        runner.conversations.lock().unwrap().remember(
            "ctx".into(),
            vec![
                RichMessage::Text {
                    role: "user".into(),
                    content: "u1".into(),
                },
                RichMessage::Text {
                    role: "agent".into(),
                    content: "a1".into(),
                },
            ],
        );
        let input = mur_common::a2a::Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "u2".into() }],
        };
        // With context → [system, prior user, prior agent, current user].
        let seeded = runner.seed_history(Some("ctx"), "SYS".into(), &input);
        assert_eq!(seeded.len(), 4);
        assert!(
            matches!(&seeded[0], RichMessage::Text { role, content } if role == "system" && content == "SYS")
        );
        assert!(matches!(&seeded[3], RichMessage::Text { role, .. } if role == "user"));
        // Without context → just system + the current user message (old behavior).
        assert_eq!(runner.seed_history(None, "SYS".into(), &input).len(), 2);
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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
        };
        let r2 = runner.clone();
        let handle = tokio::spawn(async move { r2.run_sync_streaming(spec, tx, None).await });

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

    /// A response truncated while still inside a thinking block: `stop_reason
    /// == MaxTokens` but NEITHER text NOR a tool_call was ever produced. This
    /// is what the Anthropic client now returns (instead of erroring) when
    /// the whole `max_tokens` budget goes to reasoning before any visible
    /// output starts.
    fn truncated_thinking_only_response() -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![],
            stop_reason: crate::llm::StopReason::MaxTokens,
        }
    }

    /// A response truncated in the middle of the FINAL answer: `stop_reason ==
    /// MaxTokens` with usable text and no tool_calls — the silent-corruption
    /// case from #715 (a delegated spec cut mid-word at exactly the cap).
    fn truncated_text_response(text: &str) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: text.into(),
            input_tokens: 5,
            output_tokens: 16384,
            model: "test".into(),
            tool_calls: vec![],
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

    /// A counting stub registered under the `fleet_run` wire name, for the
    /// default-Allow policy-gate tests below.
    struct CountingFleetRunTool {
        calls: Arc<AtomicU64>,
    }

    #[async_trait::async_trait]
    impl crate::tools::ToolExecutor for CountingFleetRunTool {
        fn name(&self) -> &str {
            crate::tools::fleet_run::FLEET_RUN
        }
        fn def(&self) -> crate::llm::ToolDef {
            crate::llm::ToolDef {
                name: crate::tools::fleet_run::FLEET_RUN.into(),
                description: "stub fleet_run".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }
        async fn execute(
            &self,
            _input: serde_json::Value,
        ) -> Result<String, crate::tools::ToolError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok("fleet ran".into())
        }
    }

    fn fleet_run_call_response(call_id: &str) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: call_id.into(),
                tool_name: crate::tools::fleet_run::FLEET_RUN.into(),
                input: serde_json::json!({"fleet": "deep-research"}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        }
    }

    /// issue #3: fleet_run with NO explicit rule now defaults to `Ask` (the
    /// `None => Allow` special case is gone). With an approval sink present but
    /// no responder, the 1s HITL timeout auto-denies PRE-execution — the spy's
    /// execute count MUST stay 0. This is the core issue #3 regression guard:
    /// dispatch/spend tools never run before approval.
    #[tokio::test]
    async fn fleet_run_without_rule_defaults_to_ask_and_denies_before_exec() {
        use crate::llm::stub::SequenceLlm;
        let responses: Vec<crate::llm::LlmResponse> = vec![
            fleet_run_call_response("fr-0"),
            end_turn_response("SHOULD NOT REACH"),
        ];
        let calls = Arc::new(AtomicU64::new(0));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(CountingFleetRunTool {
                    calls: calls.clone(),
                })])
                .with_tools_policy(vec![]) // no rules => default Ask
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(5),
        );
        let _ = runner.run_sync(loop_spec("fleet-run-default-ask")).await;
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "unapproved fleet_run must NOT execute (pre-exec deny)"
        );
    }

    /// issue #3: fail-closed. With NO approval sink wired at all
    /// (`pending_approvals`/`notifier` absent), an `Ask` tool must be DENIED
    /// pre-execution, never silently allowed. Spy execute count stays 0.
    #[tokio::test]
    async fn ask_tool_denies_when_no_approval_sink() {
        use crate::llm::stub::SequenceLlm;
        let responses: Vec<crate::llm::LlmResponse> =
            vec![fleet_run_call_response("fr-0"), end_turn_response("NOPE")];
        let calls = Arc::new(AtomicU64::new(0));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(CountingFleetRunTool {
                    calls: calls.clone(),
                })])
                .with_tools_policy(vec![]) // default Ask
                // NB: no with_pending_approvals / no with_notifier => no sink
                .with_hitl_timeout_secs(1)
                .with_max_iterations(5),
        );
        let _ = runner.run_sync(loop_spec("fleet-run-no-sink")).await;
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "with no approval sink, Ask must fail-closed (deny), never execute"
        );
    }

    /// issue #3: happy path — an explicit approval arriving on the pending
    /// channel lets the tool execute exactly once. A background poller pulls
    /// the sender out of `pending_approvals` and answers `allow: true`.
    #[tokio::test]
    async fn ask_tool_executes_after_approval() {
        use crate::llm::stub::SequenceLlm;
        let responses: Vec<crate::llm::LlmResponse> = vec![
            fleet_run_call_response("fr-0"),
            end_turn_response("REPORT DELIVERED"),
        ];
        let calls = Arc::new(AtomicU64::new(0));
        let pa = empty_pending_approvals();
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(CountingFleetRunTool {
                    calls: calls.clone(),
                })])
                .with_tools_policy(vec![]) // default Ask
                .with_pending_approvals(pa.clone())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(5)
                .with_max_iterations(5),
        );
        // Background approver: as soon as a pending approval appears, answer allow.
        let pa2 = pa.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..200 {
                let sender = {
                    let mut guard = pa2.lock().await;
                    guard.keys().next().cloned().and_then(|k| guard.remove(&k))
                };
                if let Some(tx) = sender {
                    let _ = tx.send(crate::hitl::HitlDecision {
                        allow: true,
                        reason: None,
                    });
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let outcome = runner.run_sync(loop_spec("fleet-run-approved")).await;
        let _ = approver.await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "approved fleet_run must execute exactly once"
        );
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(reply_text.contains("REPORT DELIVERED"), "{reply_text}");
    }

    /// An EXPLICIT Deny rule on fleet_run still wins over the built-in
    /// Allow default — the call is refused without executing.
    #[tokio::test]
    async fn fleet_run_explicit_deny_still_wins() {
        use crate::llm::stub::SequenceLlm;
        let responses: Vec<crate::llm::LlmResponse> =
            vec![fleet_run_call_response("fr-0"), end_turn_response("OK")];
        let calls = Arc::new(AtomicU64::new(0));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_tools(vec![Arc::new(CountingFleetRunTool {
                    calls: calls.clone(),
                })])
                .with_tools_policy(vec![mur_common::agent::ToolRule {
                    pattern: crate::tools::fleet_run::FLEET_RUN.into(),
                    policy: mur_common::agent::ToolPolicy::Deny,
                    risk: None,
                }])
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(5),
        );
        let _ = runner.run_sync(loop_spec("fleet-run-deny")).await;
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "explicitly denied fleet_run must never execute"
        );
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
        // no budget tripped, so usage carries token counts but NO stop_reason.
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("RECOVERED"),
            "expected the recovery turn's reply, got: {reply_text}"
        );
        let usage = task
            .usage
            .expect("usage is always populated with token counts");
        assert!(
            usage.get("stop_reason").is_none(),
            "natural end_turn after recovery must not populate a budget stop_reason; usage={usage:?}",
        );
        assert!(
            usage.get("input_tokens").is_some() && usage.get("output_tokens").is_some(),
            "usage must report real token counts; usage={usage:?}",
        );
    }

    /// Regression: a turn that burns its whole `max_tokens` budget inside a
    /// thinking block — no text, no tool_use, just `stop_reason: MaxTokens` —
    /// must recover the same way a truncated-mid-tool_use turn does, not
    /// surface a hard error to the user (this was the "invalid response:
    /// empty streamed response" crash reported from `murmur`).
    #[tokio::test]
    async fn truncated_thinking_only_injects_guidance_and_recovers() {
        use crate::llm::stub::SequenceLlm;
        let responses: Vec<crate::llm::LlmResponse> = vec![
            truncated_thinking_only_response(),
            end_turn_response("RECOVERED: produced a shorter response."),
        ];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(50),
        );
        let outcome = runner.run_sync(loop_spec("truncate-thinking")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (recovered turn), got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("RECOVERED"),
            "expected the recovery turn's reply, got: {reply_text}"
        );
    }

    /// Fix A (#715): a turn whose FINAL answer stops at `MaxTokens` (text
    /// present, no tool_calls) must not be passed off as complete — the reply
    /// gets the visible truncation marker appended and `Task.usage` carries
    /// `"truncated": true`.
    #[tokio::test]
    async fn max_tokens_final_answer_gets_marker_and_usage_flag() {
        use crate::llm::stub::SequenceLlm;
        let responses = vec![truncated_text_response("A long spec cut mid-wo")];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(5),
        );
        let outcome = runner.run_sync(loop_spec("truncate-final-answer")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.starts_with("A long spec cut mid-wo"),
            "truncated text must be preserved, got: {reply_text}"
        );
        assert!(
            reply_text.ends_with(crate::llm::MAX_TOKENS_TRUNCATION_MARKER),
            "reply must end with the visible truncation marker, got: {reply_text}"
        );
        let usage = task.usage.expect("usage is always populated");
        assert_eq!(
            usage["truncated"], true,
            "usage must flag the truncation; usage={usage:?}"
        );
    }

    /// Counterpart to the marker test: a clean end_turn must carry neither the
    /// marker nor the `truncated` usage key (the flag is additive-only).
    #[tokio::test]
    async fn clean_end_turn_has_no_truncation_marker_or_flag() {
        use crate::llm::stub::SequenceLlm;
        let responses = vec![end_turn_response("complete answer")];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(5),
        );
        let outcome = runner.run_sync(loop_spec("clean-end-turn")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            !reply_text.contains(crate::llm::MAX_TOKENS_TRUNCATION_MARKER),
            "clean turn must not carry the marker, got: {reply_text}"
        );
        let usage = task.usage.expect("usage is always populated");
        assert!(
            usage.get("truncated").is_none(),
            "clean turn must not populate the truncated flag; usage={usage:?}"
        );
    }

    /// Same marker + flag behavior on the non-agentic `run_llm` path (runner
    /// built without pending approvals — e.g. companion / plain generate).
    #[tokio::test]
    async fn run_llm_path_marks_max_tokens_truncation() {
        use crate::llm::stub::SequenceLlm;
        let responses = vec![truncated_text_response("plain reply cut mid-wo")];
        let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)));
        let outcome = runner.run_sync(loop_spec("truncate-run-llm")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.ends_with(crate::llm::MAX_TOKENS_TRUNCATION_MARKER),
            "run_llm reply must end with the truncation marker, got: {reply_text}"
        );
        let usage = task.usage.expect("usage is always populated");
        assert_eq!(
            usage["truncated"], true,
            "usage must flag the truncation; usage={usage:?}"
        );
    }

    /// Returns `InvalidResponse("empty streamed response")` on its first call,
    /// then delegates to `inner` — used to test the bounded retry for a
    /// transient empty-stream hiccup (task_runner's LLM call site).
    struct EmptyStreamOnceThenLlm {
        inner: crate::llm::stub::SequenceLlm,
        failed_once: std::sync::atomic::AtomicBool,
    }

    impl EmptyStreamOnceThenLlm {
        fn new(responses: Vec<crate::llm::LlmResponse>) -> Self {
            Self {
                inner: crate::llm::stub::SequenceLlm::new(responses),
                failed_once: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::llm::LlmClient for EmptyStreamOnceThenLlm {
        async fn generate(
            &self,
            req: crate::llm::LlmRequest,
        ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
            if !self.failed_once.swap(true, Ordering::Relaxed) {
                return Err(crate::llm::LlmError::InvalidResponse(
                    "empty streamed response".into(),
                ));
            }
            self.inner.generate(req).await
        }
        fn model_name(&self) -> &str {
            "empty-stream-once-then-stub"
        }
    }

    /// Regression: a transient empty-stream error (a momentary network/proxy
    /// hiccup) must be retried once and recover silently, not surface as a
    /// hard task error or a blank agent reply.
    #[tokio::test]
    async fn empty_stream_error_retries_once_and_recovers() {
        let responses = vec![end_turn_response("RECOVERED: retried after empty stream.")];
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(EmptyStreamOnceThenLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_max_iterations(50),
        );
        let outcome = runner.run_sync(loop_spec("empty-stream-retry")).await;
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed (recovered turn), got {outcome:?}");
        };
        let reply_text = task.messages.last().map(text_of).unwrap_or_default();
        assert!(
            reply_text.contains("RECOVERED"),
            "expected the recovery turn's reply, got: {reply_text}"
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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
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
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            active_fleet: None,
            active_team: None,
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
                RichMessage::Text { .. } | RichMessage::ImageText { .. } => {}
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

    /// #595 — graceful_exit must append the iteration-cap marker to the
    /// output text ONLY when the stop reason is `MaxIterations`, so partial
    /// execution at the cap is visible instead of looking like a clean
    /// completion. A sibling reason (`LoopDetected`) must NOT carry it.
    #[tokio::test]
    async fn graceful_exit_marks_output_only_at_iteration_cap() {
        use crate::llm::stub::SequenceLlm;
        let client: Arc<dyn crate::llm::LlmClient> = Arc::new(SequenceLlm::new(vec![
            end_turn_response("partial work done"),
            end_turn_response("partial work done"),
        ]));
        let runner = TaskRunner::with_llm(client.clone());
        let history = vec![crate::llm::RichMessage::Text {
            role: "user".into(),
            content: "do the thing".into(),
        }];

        let capped = runner
            .graceful_exit(client.as_ref(), &history, LoopStop::MaxIterations)
            .await;
        assert!(
            text_of(&capped)
                .contains("[runtime: turn ended at the iteration cap — output may be incomplete]"),
            "MaxIterations exit must carry the iteration-cap marker: {}",
            text_of(&capped)
        );

        let other = runner
            .graceful_exit(client.as_ref(), &history, LoopStop::LoopDetected)
            .await;
        assert!(
            !text_of(&other).contains("iteration cap"),
            "LoopDetected exit must NOT carry the iteration-cap marker: {}",
            text_of(&other)
        );
    }

    #[test]
    fn user_message_carries_pasted_image() {
        let msg = Message {
            role: "user".into(),
            parts: vec![
                MessagePart::Text {
                    text: "what is this?".into(),
                },
                MessagePart::Data {
                    mime_type: "image/png".into(),
                    data: serde_json::json!({ "base64": "QkFTRTY0" }),
                },
            ],
        };
        match user_message(&msg) {
            crate::llm::RichMessage::ImageText {
                media_type,
                data,
                text,
                ..
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(data, "QkFTRTY0");
                assert_eq!(text, "what is this?");
            }
            other => panic!("expected ImageText, got {other:?}"),
        }
    }

    #[test]
    fn user_message_text_only_when_no_image() {
        let msg = Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "hi".into() }],
        };
        assert!(matches!(
            user_message(&msg),
            crate::llm::RichMessage::Text { .. }
        ));
    }

    // ── Drain tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn drain_idle_runner_returns_true_immediately() {
        let runner = TaskRunner::new_stub_echo();
        // An idle runner (no in-flight tasks) must return true within the timeout.
        let ok = runner
            .await_idle(std::time::Duration::from_millis(200))
            .await;
        assert!(ok, "idle runner should drain immediately");
    }

    #[tokio::test]
    async fn drain_rejects_new_turns_after_begin_drain() {
        let runner = TaskRunner::new_stub_echo();
        runner.begin_drain();
        // New turns must be rejected with a transient Failed outcome.
        let outcome = runner.run_sync(ping_spec()).await;
        match outcome {
            TaskOutcome::Failed(task) => {
                let err = task.error.expect("drained turn must have an error");
                assert!(
                    err.recoverable,
                    "drain rejection must be marked recoverable"
                );
                assert!(
                    err.message.contains("draining"),
                    "error message must mention draining, got: {}",
                    err.message
                );
            }
            other => panic!("expected Failed after drain, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drain_still_idle_after_rejected_turn() {
        // A rejected turn must NOT register in the registry, so await_idle stays true.
        let runner = TaskRunner::new_stub_echo();
        runner.begin_drain();
        let _ = runner.run_sync(ping_spec()).await;
        let ok = runner
            .await_idle(std::time::Duration::from_millis(100))
            .await;
        assert!(
            ok,
            "registry must be clean after a rejected (draining) turn"
        );
    }

    #[tokio::test]
    async fn drain_start_async_does_not_register_working_entry() {
        // After begin_drain(), start_async must NOT leave a Working entry, so
        // await_idle returns true immediately (no phantom task blocks shutdown).
        let runner = TaskRunner::new_stub_echo();
        runner.begin_drain();
        let handle = runner.start_async(ping_spec());
        // The handle resolves to Failed (transient rejection).
        let outcome = handle.await_completion().await;
        match outcome {
            TaskOutcome::Failed(task) => {
                let err = task.error.expect("drained async turn must have an error");
                assert!(err.recoverable, "async drain rejection must be recoverable");
                assert!(
                    err.message.contains("draining"),
                    "error message must mention draining, got: {}",
                    err.message
                );
            }
            other => panic!("expected Failed from start_async after drain, got {other:?}"),
        }
        // await_idle must not hang — no Working entry was registered.
        let ok = runner
            .await_idle(std::time::Duration::from_millis(100))
            .await;
        assert!(
            ok,
            "registry must have no Working entries after start_async drain rejection"
        );
    }

    #[tokio::test]
    async fn steering_register_inject_unregister() {
        let runner = TaskRunner::new_stub_echo();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        runner.register_steering("t1", tx).await;
        runner
            .inject_steering("t1", "use ripgrep".into())
            .await
            .unwrap();
        assert_eq!(rx.recv().await.as_deref(), Some("use ripgrep"));
        // unknown task → error
        assert!(runner.inject_steering("nope", "x".into()).await.is_err());
        runner.unregister_steering("t1").await;
        assert!(runner.inject_steering("t1", "y".into()).await.is_err());
    }

    #[test]
    fn rate_limit_backoff_delay_matches_spec() {
        assert_eq!(
            rate_limit_backoff_delay(1),
            std::time::Duration::from_secs(2)
        );
        assert_eq!(
            rate_limit_backoff_delay(2),
            std::time::Duration::from_secs(4)
        );
        assert_eq!(
            rate_limit_backoff_delay(3),
            std::time::Duration::from_secs(8)
        );
    }

    #[test]
    fn rate_limit_retry_constants_are_sane() {
        // Must retry at least once for the backoff to matter, and stay small
        // enough that a still-limited account fails a turn in a bounded time
        // (2s + 4s + 8s = 14s at the current base) rather than hanging.
        assert!(MAX_RATE_LIMIT_RETRIES >= 1);
        assert!(MAX_RATE_LIMIT_RETRIES <= 5);
        assert!(RATE_LIMIT_BACKOFF_BASE >= std::time::Duration::from_millis(1));
        let max_delay = rate_limit_backoff_delay(MAX_RATE_LIMIT_RETRIES);
        assert!(max_delay <= std::time::Duration::from_secs(60));
    }

    #[test]
    fn assemble_system_prompt_appends_output_locations_rule() {
        let runner = TaskRunner::new_stub_echo().with_system_prompt(Some("BASE PROMPT".into()));
        let (sys, _fired) = runner.assemble_system_prompt("hello", None, None);
        assert!(
            sys.starts_with("BASE PROMPT"),
            "keeps the agent's own prompt first"
        );
        assert!(sys.contains("Output locations"), "injects the rule heading");
        assert!(
            sys.contains("~/.mur/artifacts/"),
            "names the run-artifact dir"
        );
        assert!(
            sys.contains("mur skill install"),
            "names the register command"
        );
    }
}

#[cfg(test)]
mod step_tests {
    use super::{STEP_MAX_BYTES, cap_step_output, step_notification};

    #[test]
    fn notification_has_jsonrpc_envelope_and_method() {
        let n = step_notification("step/started", serde_json::json!({ "step_id": "s1" }));
        assert_eq!(n["jsonrpc"], "2.0");
        assert_eq!(n["method"], "step/started");
        assert_eq!(n["params"]["step_id"], "s1");
    }

    #[test]
    fn cap_step_output_short_unchanged() {
        let (out, truncated, full_len) = cap_step_output("hello");
        assert_eq!(out, "hello");
        assert!(!truncated);
        assert_eq!(full_len, 5);
    }

    #[test]
    fn cap_step_output_long_is_truncated() {
        let big = "é".repeat(STEP_MAX_BYTES); // 2 bytes/char → over the cap
        let (out, truncated, full_len) = cap_step_output(&big);
        assert!(truncated);
        assert_eq!(full_len, big.len());
        assert!(out.is_char_boundary(out.len())); // never split a char
    }
}
