//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use crate::llm::{LlmClient, LlmMessage, LlmRequest};
use crate::skills::injector::inject_layer2;
use crate::skills::trigger_matcher::{format_layer3, layer3_body, match_prompt};
use crate::skills::RuntimeSkills;
use crate::telemetry_writer::Event;
use mur_common::a2a::{Message, MessagePart, Task, TaskState};
use mur_common::config::SkillsConfig;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
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

pub struct TaskRunner {
    backend: RunnerBackend,
    registry: Arc<Mutex<HashMap<String, TaskState>>>,
    cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    telemetry: Option<mpsc::Sender<Event>>,
    system_prompt: Option<String>,
    last_activity_at: Arc<AtomicI64>,
    skills: Option<Arc<RuntimeSkills>>,
    skills_cfg: SkillsConfig,
    recently_fired: Mutex<VecDeque<(u64, String)>>,
    turn_counter: AtomicU64,
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
            cancel_signals: Arc::new(Mutex::new(HashMap::new())),
            telemetry: None,
            system_prompt: None,
            last_activity_at: Arc::new(AtomicI64::new(0)),
            skills: None,
            skills_cfg: SkillsConfig::default(),
            recently_fired: Mutex::new(VecDeque::new()),
            turn_counter: AtomicU64::new(0),
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

    fn assemble_system_prompt(&self, user_prompt: &str) -> (String, Vec<String>) {
        let base = self.system_prompt.clone().unwrap_or_default();
        let Some(skills) = &self.skills else {
            return (base, vec![]);
        };

        let turn = self
            .turn_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let recently: HashSet<String> = {
            let q = self.recently_fired.lock().unwrap();
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

        // M2 stub for context fill: 0.0 at boot. M3 wires honest token counting.
        let ctx_fill: f64 = 0.0;
        let injection = inject_layer2(&skills.loaded, &self.skills_cfg, ctx_fill, &recently);

        let triggered = match_prompt(&skills.triggers, user_prompt);

        let mut layer3 = String::new();
        let mut suppress_names: HashSet<&str> = HashSet::new();
        for t in &triggered {
            let Some(loaded) = skills.loaded.iter().find(|s| s.name == t.skill_name) else {
                continue;
            };
            let Some(body) = layer3_body(&loaded.manifest) else {
                continue;
            };
            layer3.push('\n');
            layer3.push_str(&format_layer3(&loaded.name, loaded.trust, &body));
            suppress_names.insert(loaded.name.as_str());
            self.recently_fired
                .lock()
                .unwrap()
                .push_back((turn, loaded.name.clone()));
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
        let id = format!("task-{}", Uuid::now_v7());
        self.set_state(&id, TaskState::Working);
        let result = match &self.backend {
            RunnerBackend::StubEcho => echo_response(&spec.input),
            RunnerBackend::StubSlow => {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                echo_response(&spec.input)
            }
            RunnerBackend::Llm(client) => self.run_llm(&id, client.as_ref(), &spec.input).await,
        };
        self.set_state(&id, TaskState::Completed);
        TaskOutcome::Completed(Task {
            id,
            state: TaskState::Completed,
            messages: vec![spec.input, result],
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
            error: None,
            usage: None,
        })
    }

    pub fn start_async(&self, spec: TaskSpec) -> AsyncTaskHandle {
        self.last_activity_at
            .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
        let id = format!("task-{}", Uuid::now_v7());
        let (tx_done, rx_done) = oneshot::channel::<TaskOutcome>();
        let (tx_cancel, mut rx_cancel) = oneshot::channel::<()>();
        self.cancel_signals
            .lock()
            .unwrap()
            .insert(id.clone(), tx_cancel);
        self.set_state(&id, TaskState::Working);
        let id_clone = id.clone();
        let registry = self.registry.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    let reply = echo_response(&spec.input);
                    registry.lock().unwrap().insert(id_clone.clone(), TaskState::Completed);
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
                    registry.lock().unwrap().insert(id_clone.clone(), TaskState::Cancelled);
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
        let tx = self.cancel_signals.lock().unwrap().remove(task_id);
        match tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err(format!("task {task_id} not cancellable")),
        }
    }

    fn set_state(&self, id: &str, state: TaskState) {
        self.registry.lock().unwrap().insert(id.to_string(), state);
    }

    pub fn get_state(&self, id: &str) -> Option<TaskState> {
        self.registry.lock().unwrap().get(id).cloned()
    }

    /// Unix timestamp of the last `start_async` call. Returns 0 if no task has been started.
    pub fn last_activity_at(&self) -> i64 {
        self.last_activity_at.load(Ordering::Relaxed)
    }

    async fn run_llm(&self, task_id: &str, client: &dyn LlmClient, input: &Message) -> Message {
        let prompt = text_of(input);
        let mut messages: Vec<LlmMessage> = Vec::new();

        let (system, _fired) = self.assemble_system_prompt(&prompt);
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
        match client.generate(req).await {
            Ok(resp) => {
                let latency_ms = start.elapsed().as_millis() as u64;
                if let Some(tx) = &self.telemetry {
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
                        })
                        .await;
                }
                Message {
                    role: "agent".into(),
                    parts: vec![MessagePart::Text { text: resp.text }],
                }
            }
            Err(e) => Message {
                role: "agent".into(),
                parts: vec![MessagePart::Text {
                    text: format!("llm error: {e}"),
                }],
            },
        }
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
        .filter(|line| !names.iter().any(|n| line.contains(&format!("[Skill: {n} "))))
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
}
