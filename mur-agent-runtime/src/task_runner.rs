//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use mur_common::a2a::{Message, MessagePart, Task, TaskState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
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
    // Llm(Arc<dyn LLMClient>) — Task 16 plugs real backend
}

pub struct TaskRunner {
    backend: RunnerBackend,
    registry: Arc<Mutex<HashMap<String, TaskState>>>,
    cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

impl TaskRunner {
    pub fn new_stub_echo() -> Self {
        Self::with_backend(RunnerBackend::StubEcho)
    }

    pub fn new_stub_slow() -> Self {
        Self::with_backend(RunnerBackend::StubSlow)
    }

    pub fn with_backend(backend: RunnerBackend) -> Self {
        Self {
            backend,
            registry: Arc::new(Mutex::new(HashMap::new())),
            cancel_signals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run_sync(&self, spec: TaskSpec) -> TaskOutcome {
        let id = format!("task-{}", Uuid::now_v7());
        self.set_state(&id, TaskState::Working);
        let result = match self.backend {
            RunnerBackend::StubEcho => echo_response(&spec.input),
            RunnerBackend::StubSlow => {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                echo_response(&spec.input)
            }
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
