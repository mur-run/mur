//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use crate::hooks::{HookChain, HookCtx, PromptView};
use crate::llm::{LlmClient, LlmMessage, LlmRequest};
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
}

impl TaskRunner {
    pub fn new_stub_echo() -> Self {
        Self::with_backend(RunnerBackend::StubEcho)
    }

    pub fn new_stub_slow() -> Self {
        Self::with_backend(RunnerBackend::StubSlow)
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
            let inventory = McpInventory::default(); // TODO: wire to MCP registry
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
        let id = format!("task-{}", Uuid::now_v7());
        self.set_state(&id, TaskState::Working);
        let result: Result<Message, TaskError> = match &self.backend {
            RunnerBackend::StubEcho => Ok(echo_response(&spec.input)),
            RunnerBackend::StubSlow => {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(echo_response(&spec.input))
            }
            RunnerBackend::Llm(client) => {
                self.run_llm(&id, client.as_ref(), &spec.input, sink).await
            }
        };
        let now = chrono::Utc::now().to_rfc3339();
        match result {
            Ok(reply) => {
                self.set_state(&id, TaskState::Completed);
                TaskOutcome::Completed(Task {
                    id,
                    state: TaskState::Completed,
                    messages: vec![spec.input, reply],
                    created_at: now.clone(),
                    completed_at: Some(now),
                    error: None,
                    usage: None,
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
        let mut messages: Vec<LlmMessage> = Vec::new();

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
            messages.push(LlmMessage {
                role: "system".into(),
                content: system,
            });
        }

        messages.push(LlmMessage {
            role: input.role.clone(),
            content: prompt,
        });
        let req = LlmRequest {
            messages,
            temperature: None,
            max_tokens: None,
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
}
