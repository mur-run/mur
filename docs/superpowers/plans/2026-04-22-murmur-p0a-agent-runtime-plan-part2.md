# murmur P0a — Implementation Plan (Part 2)

> Continuation of `2026-04-22-murmur-p0a-agent-runtime-plan.md`. Tasks 7-40.

---

## Task 7: Telemetry JSONL writer + notification builder

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/telemetry_writer.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/telemetry.rs`

- [ ] **Step 1: Write failing tests**

Create `mur-agent-runtime/tests/telemetry.rs`:

```rust
use mur_agent_runtime::telemetry_writer::{TelemetryWriter, Event};
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn llm_call_event_appends_jsonl_and_emits_notification() {
    let tmp = TempDir::new().unwrap();
    let (writer, mut out_rx) = TelemetryWriter::new(tmp.path().to_path_buf(), "agent_a".into(), "uuid-x".into()).await.unwrap();
    writer.emit(Event::LlmCall {
        trace_id: "t1".into(), task_id: "task-1".into(),
        model: "llama3.2".into(), input_tokens: 100, output_tokens: 50,
        latency_ms: 100, cost_usd: 0.0, provider: "ollama".into(),
    }).await;
    writer.flush().await;

    // File written
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let file_path = tmp.path().join(format!("{today}.jsonl"));
    let contents = std::fs::read_to_string(&file_path).unwrap();
    assert!(contents.contains("\"gen_ai.request.model\":\"llama3.2\""));
    assert!(contents.contains("\"mur.agent.name\":\"agent_a\""));

    // Notification emitted on channel
    let notif = out_rx.recv().await.unwrap();
    assert_eq!(notif["method"], json!("telemetry/llm_call"));
    assert_eq!(notif["params"]["mur.task.id"], json!("task-1"));
}

#[tokio::test]
async fn error_event_has_kind_field() {
    let tmp = TempDir::new().unwrap();
    let (writer, mut rx) = TelemetryWriter::new(tmp.path().to_path_buf(), "agent_a".into(), "uuid-x".into()).await.unwrap();
    writer.emit(Event::Error {
        kind: "llm_rate_limit".into(), message: "429".into(),
        task_id: Some("task-1".into()), recoverable: true,
    }).await;
    let notif = rx.recv().await.unwrap();
    assert_eq!(notif["params"]["kind"], json!("llm_rate_limit"));
}
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p mur-agent-runtime --test telemetry`
Expected: compile failure.

- [ ] **Step 3: Implement `telemetry_writer.rs`**

Write `mur-agent-runtime/src/telemetry_writer.rs`:

```rust
//! OTel GenAI + mur.* JSONL writer, with notification side-channel
//! so the stdio/socket transport can stream notifications to callers.

use mur_common::telemetry::*;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

const CHANNEL_BUF: usize = 256;

#[derive(Debug, Clone)]
pub enum Event {
    LlmCall { trace_id: String, task_id: String, model: String,
              input_tokens: u64, output_tokens: u64, latency_ms: u64,
              cost_usd: f64, provider: String },
    ToolCall { trace_id: String, task_id: String, mcp_server: String,
               tool: String, duration_ms: u64, ok: bool },
    Error { kind: String, message: String, task_id: Option<String>, recoverable: bool },
    Warning { kind: String, message: String },
    Heartbeat { uptime_s: u64, mem_mb: u64, active_tasks: u32 },
    TaskProgress { task_id: String, stage: String, message: Option<String>, percent: Option<u8> },
}

pub struct TelemetryWriter {
    tx: mpsc::Sender<Event>,
}

impl TelemetryWriter {
    /// Spawn the background writer task. Returns a handle that submits events
    /// plus a receiver that downstream transports subscribe to for live
    /// notification forwarding.
    pub async fn new(dir: PathBuf, agent_name: String, agent_uuid: String)
        -> std::io::Result<(Self, mpsc::Receiver<Value>)>
    {
        fs::create_dir_all(&dir).await?;
        let (in_tx, mut in_rx) = mpsc::channel::<Event>(CHANNEL_BUF);
        let (out_tx, out_rx) = mpsc::channel::<Value>(CHANNEL_BUF);
        tokio::spawn(async move {
            while let Some(ev) = in_rx.recv().await {
                let notif = event_to_notification(&ev, &agent_name, &agent_uuid);
                // Write to daily JSONL
                let today = chrono::Utc::now().format("%Y-%m-%d");
                let path = dir.join(format!("{today}.jsonl"));
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path).await {
                    let line = format!("{}\n", serde_json::to_string(&notif["params"]).unwrap_or_default());
                    let _ = f.write_all(line.as_bytes()).await;
                }
                // Forward as notification (best-effort)
                let _ = out_tx.send(notif).await;
            }
        });
        Ok((Self { tx: in_tx }, out_rx))
    }

    pub async fn emit(&self, ev: Event) {
        let _ = self.tx.send(ev).await;
    }

    pub async fn flush(&self) {
        // send+recv in channel order guarantees prior events are queued.
        // Small sleep lets the writer task drain; acceptable for tests.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn event_to_notification(ev: &Event, name: &str, uuid: &str) -> Value {
    let mut params = json!({
        MUR_AGENT_NAME: name,
        MUR_AGENT_UUID: uuid,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let method = match ev {
        Event::LlmCall { trace_id, task_id, model, input_tokens, output_tokens,
                         latency_ms, cost_usd, provider } => {
            params[GEN_AI_SYSTEM] = json!(provider);
            params[GEN_AI_REQUEST_MODEL] = json!(model);
            params[GEN_AI_USAGE_INPUT_TOKENS] = json!(input_tokens);
            params[GEN_AI_USAGE_OUTPUT_TOKENS] = json!(output_tokens);
            params["latency_ms"] = json!(latency_ms);
            params["cost_usd"] = json!(cost_usd);
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            METHOD_LLM_CALL
        }
        Event::ToolCall { trace_id, task_id, mcp_server, tool, duration_ms, ok } => {
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            params[MUR_MCP_SERVER] = json!(mcp_server);
            params["tool"] = json!(tool);
            params["duration_ms"] = json!(duration_ms);
            params["ok"] = json!(ok);
            METHOD_TOOL_CALL
        }
        Event::Error { kind, message, task_id, recoverable } => {
            params["kind"] = json!(kind);
            params["message"] = json!(message);
            params["recoverable"] = json!(recoverable);
            if let Some(t) = task_id { params[MUR_TASK_ID] = json!(t); }
            METHOD_ERROR
        }
        Event::Warning { kind, message } => {
            params["kind"] = json!(kind);
            params["message"] = json!(message);
            METHOD_WARNING
        }
        Event::Heartbeat { uptime_s, mem_mb, active_tasks } => {
            params["uptime_s"] = json!(uptime_s);
            params["mem_mb"] = json!(mem_mb);
            params["active_tasks"] = json!(active_tasks);
            METHOD_HEARTBEAT
        }
        Event::TaskProgress { task_id, stage, message, percent } => {
            params[MUR_TASK_ID] = json!(task_id);
            params["stage"] = json!(stage);
            if let Some(m) = message { params["message"] = json!(m); }
            if let Some(p) = percent { params["percent"] = json!(p); }
            METHOD_TASK_PROGRESS
        }
    };
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test telemetry`
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/telemetry_writer.rs mur-agent-runtime/tests/telemetry.rs
git commit -m "feat(agent-runtime): telemetry JSONL writer + notification channel"
```

---

## Task 8: JSON-RPC 2.0 dispatch + error code mapping

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/protocol/a2a_server.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/a2a_dispatch.rs`

- [ ] **Step 1: Write failing tests**

```rust
// tests/a2a_dispatch.rs
use mur_agent_runtime::protocol::a2a_server::{Dispatcher, MethodHandler, HandlerError};
use mur_common::{JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Value};
use async_trait::async_trait;

struct Echo;
#[async_trait]
impl MethodHandler for Echo {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError> {
        Ok(params.unwrap_or(json!(null)))
    }
}

#[tokio::test]
async fn dispatches_known_method() {
    let mut d = Dispatcher::new();
    d.register("echo", Box::new(Echo));
    let req = JsonRpcRequest { jsonrpc: "2.0".into(), id: Some(json!(1)), method: "echo".into(), params: Some(json!("hi")) };
    let resp = d.dispatch(req).await.unwrap();
    assert_eq!(resp.result, Some(json!("hi")));
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn returns_method_not_found_with_code_neg32601() {
    let d = Dispatcher::new();
    let req = JsonRpcRequest { jsonrpc: "2.0".into(), id: Some(json!(2)), method: "nope".into(), params: None };
    let resp = d.dispatch(req).await.unwrap();
    assert_eq!(resp.error.as_ref().unwrap().code, -32601);
}

#[tokio::test]
async fn maps_handler_error_to_custom_code() {
    struct Fails;
    #[async_trait]
    impl MethodHandler for Fails {
        async fn handle(&self, _p: Option<Value>) -> Result<Value, HandlerError> {
            Err(HandlerError::CommunicationDenied("caller_x".into()))
        }
    }
    let mut d = Dispatcher::new();
    d.register("x", Box::new(Fails));
    let req = JsonRpcRequest { jsonrpc: "2.0".into(), id: Some(json!(3)), method: "x".into(), params: None };
    let resp = d.dispatch(req).await.unwrap();
    assert_eq!(resp.error.as_ref().unwrap().code, -32011);
}
```

Add `async-trait = "0.1"` to `mur-agent-runtime/Cargo.toml` dependencies.

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p mur-agent-runtime --test a2a_dispatch`
Expected: compile failure.

- [ ] **Step 3: Implement `protocol/a2a_server.rs`**

```rust
//! JSON-RPC 2.0 dispatch + error code mapping (spec §8.8).

use async_trait::async_trait;
use mur_common::{JsonRpcRequest, JsonRpcResponse, JsonRpcError};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("parse error: {0}")] ParseError(String),
    #[error("invalid request: {0}")] InvalidRequest(String),
    #[error("invalid params: {0}")] InvalidParams(String),
    #[error("internal: {0}")] Internal(String),
    #[error("task not found: {0}")] TaskNotFound(String),
    #[error("task already completed: {0}")] TaskAlreadyCompleted(String),
    #[error("task cancelled: {0}")] TaskCancelled(String),
    #[error("capability not supported: {0}")] UnsupportedCapability(String),
    #[error("communication denied: {0}")] CommunicationDenied(String),
}

impl HandlerError {
    pub fn code(&self) -> i32 {
        match self {
            Self::ParseError(_) => -32700,
            Self::InvalidRequest(_) => -32600,
            Self::InvalidParams(_) => -32602,
            Self::Internal(_) => -32603,
            Self::TaskNotFound(_) => -32000,
            Self::TaskAlreadyCompleted(_) => -32001,
            Self::TaskCancelled(_) => -32002,
            Self::UnsupportedCapability(_) => -32010,
            Self::CommunicationDenied(_) => -32011,
        }
    }
}

#[async_trait]
pub trait MethodHandler: Send + Sync {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError>;
}

pub struct Dispatcher {
    methods: HashMap<String, Box<dyn MethodHandler>>,
}

impl Dispatcher {
    pub fn new() -> Self { Self { methods: HashMap::new() } }

    pub fn register(&mut self, name: &str, handler: Box<dyn MethodHandler>) {
        self.methods.insert(name.to_string(), handler);
    }

    pub async fn dispatch(&self, req: JsonRpcRequest) -> Result<JsonRpcResponse, HandlerError> {
        let id = req.id.clone().unwrap_or(json!(null));
        if req.jsonrpc != "2.0" {
            return Ok(Self::err_response(id, -32600, "jsonrpc must be '2.0'"));
        }
        match self.methods.get(&req.method) {
            Some(handler) => match handler.handle(req.params).await {
                Ok(result) => Ok(JsonRpcResponse {
                    jsonrpc: "2.0".into(), id, result: Some(result), error: None,
                }),
                Err(e) => Ok(Self::err_response(id, e.code(), &e.to_string())),
            },
            None => Ok(Self::err_response(id, -32601, &format!("method not found: {}", req.method))),
        }
    }

    fn err_response(id: Value, code: i32, message: &str) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.to_string(), data: None }),
        }
    }
}
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test a2a_dispatch`
Expected: three tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/a2a_server.rs mur-agent-runtime/tests/a2a_dispatch.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(agent-runtime): JSON-RPC 2.0 dispatcher with A2A error codes"
```

---

## Task 9: `agent/card` method

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/protocol/methods/card.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/card_method.rs`

- [ ] **Step 1: Write failing test**

```rust
// tests/card_method.rs
use mur_agent_runtime::protocol::methods::card::CardHandler;
use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use std::sync::Arc;

#[tokio::test]
async fn card_returns_agent_identity_and_card_fields() {
    let profile = load_test_profile();
    let handler = CardHandler::new(Arc::new(profile));
    let result = handler.handle(None).await.unwrap();
    assert_eq!(result["name"], "agent_a");
    assert_eq!(result["protocolVersion"], "a2a/0.3");
    assert!(result["capabilities"].as_array().unwrap().iter().any(|c| c == "a2a.message.send"));
    assert_eq!(result["transports"].as_array().unwrap()[0], "stdio");
    assert!(result["entitlements"].is_object(), "entitlements must be exposed on card");
}

fn load_test_profile() -> mur_agent_runtime::profile::Profile {
    use tempfile::TempDir;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_a");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), include_str!("fixtures/profile_minimal.yaml")).unwrap();
    let p = mur_agent_runtime::profile::Profile::load(&dir).unwrap();
    // leak tmp so the profile's agent_home stays valid for the test
    std::mem::forget(tmp);
    p
}
```

Create `mur-agent-runtime/tests/fixtures/profile_minimal.yaml` — same YAML as `profile_unrestricted.yaml` but with name `agent_a` and `outbound.mode: restricted`.

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p mur-agent-runtime --test card_method`
Expected: compile failure.

- [ ] **Step 3: Implement `protocol/methods/card.rs`**

```rust
//! agent/card method — project AgentProfile into an A2A Agent Card.

use async_trait::async_trait;
use crate::profile::Profile;
use crate::protocol::a2a_server::{MethodHandler, HandlerError};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct CardHandler { profile: Arc<Profile> }

impl CardHandler {
    pub fn new(profile: Arc<Profile>) -> Self { Self { profile } }
}

#[async_trait]
impl MethodHandler for CardHandler {
    async fn handle(&self, _params: Option<Value>) -> Result<Value, HandlerError> {
        let p = &self.profile.inner;
        let mut transports = vec![];
        if p.transport.stdio { transports.push("stdio"); }
        if p.transport.socket.enabled && p.transport.socket.bind.starts_with("unix://") {
            transports.push("unix-socket");
        }
        Ok(json!({
            "protocolVersion": "a2a/0.3",
            "name": p.name,
            "id": p.id,
            "displayName": p.display_name,
            "version": p.version,
            "description": p.persona.description,
            "capabilities": p.capabilities,
            "transports": transports,
            "endpoints": {
                "stdio": "pipe://self",
                "unix-socket": p.transport.socket.bind,
            },
            "persona": {
                "category": p.persona.category,
                "traits": p.persona.traits,
            },
            "skills": p.skills.iter().map(|s| json!({"id": s})).collect::<Vec<_>>(),
            "entitlements": p.entitlements,
        }))
    }
}
```

- [ ] **Step 4: Run test — expect pass**

Run: `cargo test -p mur-agent-runtime --test card_method`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/methods/card.rs mur-agent-runtime/tests/card_method.rs mur-agent-runtime/tests/fixtures/profile_minimal.yaml
git commit -m "feat(agent-runtime): agent/card method returns A2A Agent Card"
```

---

## Task 10: Task state machine + `message/send` orchestration skeleton

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/task_runner.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/task_runner.rs`

- [ ] **Step 1: Write failing tests**

```rust
// tests/task_runner.rs
use mur_agent_runtime::task_runner::{TaskRunner, TaskSpec, TaskOutcome};
use mur_common::a2a::{Message, MessagePart, TaskState};

#[tokio::test]
async fn sync_task_reaches_completed_state() {
    let runner = TaskRunner::new_stub_echo();
    let spec = TaskSpec {
        input: Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "ping".into() }],
        },
        context_task_id: None,
    };
    let outcome = runner.run_sync(spec).await;
    match outcome {
        TaskOutcome::Completed(task) => {
            assert_eq!(task.state, TaskState::Completed);
            assert!(task.messages.iter().any(|m| m.role == "agent"));
        }
        other => panic!("expected Completed, got {other:?}"),
    }
}

#[tokio::test]
async fn cancellation_transitions_to_cancelled() {
    let runner = TaskRunner::new_stub_slow();
    let spec = TaskSpec {
        input: Message { role: "user".into(), parts: vec![MessagePart::Text { text: "slow".into() }] },
        context_task_id: None,
    };
    let handle = runner.start_async(spec);
    let task_id = handle.task_id().to_string();
    runner.cancel(&task_id).await.unwrap();
    let outcome = handle.await_completion().await;
    assert!(matches!(outcome, TaskOutcome::Cancelled(_)));
}
```

- [ ] **Step 2: Run tests — expect fail**

Run: `cargo test -p mur-agent-runtime --test task_runner`
Expected: compile failure.

- [ ] **Step 3: Implement `task_runner.rs`**

```rust
//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use mur_common::a2a::{Message, MessagePart, Task, TaskState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TaskSpec { pub input: Message, pub context_task_id: Option<String> }

#[derive(Debug)]
pub enum TaskOutcome { Completed(Task), Failed(Task), Cancelled(Task) }

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
    pub fn new_stub_echo() -> Self { Self::with_backend(RunnerBackend::StubEcho) }
    pub fn new_stub_slow() -> Self { Self::with_backend(RunnerBackend::StubSlow) }

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
            id, state: TaskState::Completed,
            messages: vec![spec.input, result],
            created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
            error: None, usage: None,
        })
    }

    pub fn start_async(&self, spec: TaskSpec) -> AsyncTaskHandle {
        let id = format!("task-{}", Uuid::now_v7());
        let (tx_done, rx_done) = oneshot::channel::<TaskOutcome>();
        let (tx_cancel, mut rx_cancel) = oneshot::channel::<()>();
        self.cancel_signals.lock().unwrap().insert(id.clone(), tx_cancel);
        self.set_state(&id, TaskState::Working);
        let id_clone = id.clone();
        let backend = self.backend.clone();
        let registry = self.registry.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {
                    let _ = match backend {
                        RunnerBackend::StubSlow | RunnerBackend::StubEcho => {}
                    };
                    let reply = echo_response(&spec.input);
                    registry.lock().unwrap().insert(id_clone.clone(), TaskState::Completed);
                    let _ = tx_done.send(TaskOutcome::Completed(Task {
                        id: id_clone.clone(), state: TaskState::Completed,
                        messages: vec![spec.input, reply],
                        created_at: chrono::Utc::now().to_rfc3339(),
                        completed_at: Some(chrono::Utc::now().to_rfc3339()),
                        error: None, usage: None,
                    }));
                }
                _ = &mut rx_cancel => {
                    registry.lock().unwrap().insert(id_clone.clone(), TaskState::Cancelled);
                    let _ = tx_done.send(TaskOutcome::Cancelled(Task {
                        id: id_clone.clone(), state: TaskState::Cancelled,
                        messages: vec![spec.input], created_at: chrono::Utc::now().to_rfc3339(),
                        completed_at: Some(chrono::Utc::now().to_rfc3339()),
                        error: None, usage: None,
                    }));
                }
            }
        });
        AsyncTaskHandle { id, done: rx_done }
    }

    pub async fn cancel(&self, task_id: &str) -> Result<(), String> {
        let tx = self.cancel_signals.lock().unwrap().remove(task_id);
        match tx {
            Some(tx) => { let _ = tx.send(()); Ok(()) }
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
    pub fn task_id(&self) -> &str { &self.id }
    pub async fn await_completion(self) -> TaskOutcome {
        self.done.await.unwrap_or_else(|_| TaskOutcome::Failed(Task {
            id: self.id, state: TaskState::Failed,
            messages: vec![], created_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None, error: None, usage: None,
        }))
    }
}

fn echo_response(input: &Message) -> Message {
    let text = input.parts.iter().find_map(|p| match p {
        MessagePart::Text { text } => Some(text.clone()),
        _ => None,
    }).unwrap_or_default();
    Message { role: "agent".into(), parts: vec![MessagePart::Text { text: format!("echo: {text}") }] }
}
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test task_runner`
Expected: both tests pass (cancellation test completes in <1s even though slow stub is 60s — cancel fires immediately).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs mur-agent-runtime/tests/task_runner.rs
git commit -m "feat(agent-runtime): task state machine with sync + async orchestration"
```

---

## Task 11: `message/send`, `tasks/get`, `tasks/cancel`, `tasks/list` methods

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/protocol/methods/message_send.rs`
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/protocol/methods/tasks.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/methods.rs`

- [ ] **Step 1: Write failing tests**

```rust
// tests/methods.rs
use mur_agent_runtime::protocol::methods::{message_send::MessageSendHandler, tasks::{TasksGetHandler, TasksCancelHandler, TasksListHandler}};
use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use mur_agent_runtime::task_runner::TaskRunner;
use std::sync::Arc;
use serde_json::json;

#[tokio::test]
async fn message_send_returns_completed_task() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = MessageSendHandler::new(runner);
    let result = h.handle(Some(json!({"message": {"role":"user","parts":[{"kind":"text","text":"hi"}]}}))).await.unwrap();
    assert_eq!(result["state"], "completed");
}

#[tokio::test]
async fn tasks_get_returns_not_found_for_unknown_id() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksGetHandler::new(runner);
    let err = h.handle(Some(json!({"id": "task-ghost"}))).await.unwrap_err();
    assert_eq!(err.code(), -32000);
}

#[tokio::test]
async fn tasks_cancel_on_unknown_returns_not_found() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksCancelHandler::new(runner);
    let err = h.handle(Some(json!({"id": "task-ghost"}))).await.unwrap_err();
    assert_eq!(err.code(), -32000);
}

#[tokio::test]
async fn tasks_list_returns_array() {
    let runner = Arc::new(TaskRunner::new_stub_echo());
    let h = TasksListHandler::new(runner);
    let result = h.handle(None).await.unwrap();
    assert!(result.is_array());
}
```

- [ ] **Step 2: Run tests — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test methods`
Expected: compile failure.

- [ ] **Step 3: Implement the four methods**

`src/protocol/methods/message_send.rs`:

```rust
use async_trait::async_trait;
use crate::protocol::a2a_server::{MethodHandler, HandlerError};
use crate::task_runner::{TaskRunner, TaskSpec, TaskOutcome};
use mur_common::a2a::{Message, MessagePart};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct MessageSendHandler { runner: Arc<TaskRunner> }
impl MessageSendHandler { pub fn new(runner: Arc<TaskRunner>) -> Self { Self { runner } } }

#[async_trait]
impl MethodHandler for MessageSendHandler {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let message: Message = serde_json::from_value(p["message"].clone())
            .map_err(|e| HandlerError::InvalidParams(format!("message: {e}")))?;
        let context_task_id = p.get("context").and_then(|c| c.get("task_id"))
            .and_then(|v| v.as_str()).map(|s| s.to_string());
        let spec = TaskSpec { input: message, context_task_id };
        match self.runner.run_sync(spec).await {
            TaskOutcome::Completed(task) | TaskOutcome::Failed(task) | TaskOutcome::Cancelled(task) => {
                Ok(serde_json::to_value(&task).unwrap())
            }
        }
    }
}
```

`src/protocol/methods/tasks.rs`:

```rust
use async_trait::async_trait;
use crate::protocol::a2a_server::{MethodHandler, HandlerError};
use crate::task_runner::TaskRunner;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct TasksGetHandler { runner: Arc<TaskRunner> }
impl TasksGetHandler { pub fn new(r: Arc<TaskRunner>) -> Self { Self { runner: r } } }

#[async_trait]
impl MethodHandler for TasksGetHandler {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError> {
        let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str().map(String::from)))
            .ok_or_else(|| HandlerError::InvalidParams("missing id".into()))?;
        match self.runner.get_state(&id) {
            Some(state) => Ok(json!({"id": id, "state": state})),
            None => Err(HandlerError::TaskNotFound(id)),
        }
    }
}

pub struct TasksCancelHandler { runner: Arc<TaskRunner> }
impl TasksCancelHandler { pub fn new(r: Arc<TaskRunner>) -> Self { Self { runner: r } } }

#[async_trait]
impl MethodHandler for TasksCancelHandler {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError> {
        let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str().map(String::from)))
            .ok_or_else(|| HandlerError::InvalidParams("missing id".into()))?;
        self.runner.cancel(&id).await
            .map_err(|_| HandlerError::TaskNotFound(id.clone()))?;
        Ok(json!({"id": id, "state": "cancelled"}))
    }
}

pub struct TasksListHandler { _runner: Arc<TaskRunner> }
impl TasksListHandler { pub fn new(r: Arc<TaskRunner>) -> Self { Self { _runner: r } } }

#[async_trait]
impl MethodHandler for TasksListHandler {
    async fn handle(&self, _params: Option<Value>) -> Result<Value, HandlerError> {
        // P0a: return empty list; full history comes when we persist TaskRunner state
        Ok(json!([]))
    }
}
```

- [ ] **Step 4: Run tests — expect pass**

Run: `cargo test -p mur-agent-runtime --test methods`
Expected: four tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/protocol/methods/message_send.rs mur-agent-runtime/src/protocol/methods/tasks.rs mur-agent-runtime/tests/methods.rs
git commit -m "feat(agent-runtime): message/send + tasks/* method handlers"
```

---

## Task 12: Stdio transport (newline-delimited JSON)

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/transport/stdio.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/transport_stdio.rs`

- [ ] **Step 1: Write failing test**

```rust
// tests/transport_stdio.rs
use mur_agent_runtime::transport::stdio::serve_stdio;
use mur_agent_runtime::protocol::a2a_server::Dispatcher;
use tokio::io::{duplex, AsyncWriteExt, AsyncReadExt, AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use serde_json::json;

#[tokio::test]
async fn serves_dispatch_on_stdio_pipe() {
    let (mut client, server) = duplex(65536);
    let (server_read, server_write) = tokio::io::split(server);
    let (notif_tx, notif_rx) = mpsc::channel(16);
    let dispatcher = {
        let mut d = Dispatcher::new();
        use async_trait::async_trait;
        use mur_agent_runtime::protocol::a2a_server::{MethodHandler, HandlerError};
        struct Ping;
        #[async_trait]
        impl MethodHandler for Ping {
            async fn handle(&self, _: Option<serde_json::Value>) -> Result<serde_json::Value, HandlerError> {
                Ok(serde_json::json!({"pong": true}))
            }
        }
        d.register("ping", Box::new(Ping));
        d
    };
    tokio::spawn(serve_stdio(dispatcher, server_read, server_write, notif_rx));
    let req = json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string() + "\n";
    client.write_all(req.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["result"]["pong"], true);
    drop(notif_tx);
}
```

- [ ] **Step 2: Run — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test transport_stdio`

- [ ] **Step 3: Implement `transport/stdio.rs`**

```rust
//! Newline-delimited JSON-RPC 2.0 over stdio.
use crate::protocol::a2a_server::Dispatcher;
use mur_common::JsonRpcRequest;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

pub async fn serve_stdio<R, W>(
    dispatcher: Dispatcher,
    reader: R,
    writer: W,
    mut notifications: mpsc::Receiver<Value>,
) -> std::io::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));
    let w_notif = writer.clone();
    tokio::spawn(async move {
        while let Some(notif) = notifications.recv().await {
            let line = format!("{notif}\n");
            let mut w = w_notif.lock().await;
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.flush().await;
        }
    });

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 { break; }
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => continue,  // silently drop malformed; Task 8's dispatcher also guards
        };
        let resp = dispatcher.dispatch(req).await.unwrap();
        let out = format!("{}\n", serde_json::to_string(&resp).unwrap());
        let mut w = writer.lock().await;
        w.write_all(out.as_bytes()).await?;
        w.flush().await?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run test — expect pass**

Run: `cargo test -p mur-agent-runtime --test transport_stdio`

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/transport/stdio.rs mur-agent-runtime/tests/transport_stdio.rs
git commit -m "feat(agent-runtime): stdio transport with newline-delimited JSON-RPC"
```

---

## Task 13: Unix socket transport + SO_PEERCRED

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/transport/unix_socket.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/transport_unix.rs`

- [ ] **Step 1: Write failing test**

```rust
// tests/transport_unix.rs (Unix only)
#![cfg(unix)]
use mur_agent_runtime::transport::unix_socket::{serve_unix, PeerInfo};
use mur_agent_runtime::protocol::a2a_server::Dispatcher;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tempfile::TempDir;
use tokio::sync::mpsc;
use serde_json::json;

#[tokio::test]
async fn roundtrip_over_unix_socket() {
    let tmp = TempDir::new().unwrap();
    let sock_path = tmp.path().join("a.sock");
    let (notif_tx, notif_rx) = mpsc::channel(16);
    let dispatcher = {
        use async_trait::async_trait;
        use mur_agent_runtime::protocol::a2a_server::{MethodHandler, HandlerError};
        let mut d = Dispatcher::new();
        struct Ping;
        #[async_trait]
        impl MethodHandler for Ping {
            async fn handle(&self, _: Option<serde_json::Value>) -> Result<serde_json::Value, HandlerError> {
                Ok(serde_json::json!({"pong": true}))
            }
        }
        d.register("ping", Box::new(Ping));
        d
    };
    let path = sock_path.clone();
    tokio::spawn(async move {
        let _ = serve_unix(dispatcher, path, notif_rx).await;
    });
    // Allow server to bind
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let stream = UnixStream::connect(&sock_path).await.unwrap();
    let (read, mut write) = stream.into_split();
    let req = json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string() + "\n";
    write.write_all(req.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["result"]["pong"], true);
    drop(notif_tx);
}
```

- [ ] **Step 2: Run — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test transport_unix`

- [ ] **Step 3: Implement `transport/unix_socket.rs`**

```rust
//! Unix domain socket transport — JSON-RPC 2.0 newline-delimited,
//! with SO_PEERCRED caller resolution (Task 22 consumes this).

use crate::protocol::a2a_server::Dispatcher;
use mur_common::JsonRpcRequest;
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub struct PeerInfo { pub pid: u32, pub uid: u32 }

pub async fn serve_unix(
    dispatcher: Dispatcher,
    path: PathBuf,
    mut notifications: mpsc::Receiver<Value>,
) -> std::io::Result<()> {
    if path.exists() { let _ = std::fs::remove_file(&path); }
    let listener = UnixListener::bind(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    let dispatcher = std::sync::Arc::new(dispatcher);
    // Broadcast channel for notifications across all connected peers
    let (bcast_tx, _) = tokio::sync::broadcast::channel::<Value>(256);
    let bcast_forward = bcast_tx.clone();
    tokio::spawn(async move {
        while let Some(n) = notifications.recv().await {
            let _ = bcast_forward.send(n);
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let peer = peer_info(&stream);
        let dispatcher = dispatcher.clone();
        let mut bcast_rx = bcast_tx.subscribe();
        tokio::spawn(async move {
            let (read, write) = stream.into_split();
            let write = std::sync::Arc::new(tokio::sync::Mutex::new(write));
            let w_notif = write.clone();
            let notif_task = tokio::spawn(async move {
                while let Ok(n) = bcast_rx.recv().await {
                    let line = format!("{n}\n");
                    let mut w = w_notif.lock().await;
                    if w.write_all(line.as_bytes()).await.is_err() { break; }
                    let _ = w.flush().await;
                }
            });
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line).await.unwrap_or(0);
                if n == 0 { break; }
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r, Err(_) => continue,
                };
                let resp = dispatcher.dispatch(req).await.unwrap();
                let out = format!("{}\n", serde_json::to_string(&resp).unwrap());
                let mut w = write.lock().await;
                if w.write_all(out.as_bytes()).await.is_err() { break; }
                let _ = w.flush().await;
            }
            let _ = peer;                    // passed to auth / communication_policy via request context in Task 22
            notif_task.abort();
        });
    }
}

#[cfg(target_os = "linux")]
fn peer_info(stream: &tokio::net::UnixStream) -> Option<PeerInfo> {
    use std::os::unix::io::AsRawFd;
    use std::mem;
    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED,
                         &mut cred as *mut _ as *mut _, &mut len)
    };
    if rc == 0 { Some(PeerInfo { pid: cred.pid as u32, uid: cred.uid }) } else { None }
}

#[cfg(target_os = "macos")]
fn peer_info(stream: &tokio::net::UnixStream) -> Option<PeerInfo> {
    use std::os::unix::io::AsRawFd;
    use std::mem;
    let fd = stream.as_raw_fd();
    let mut cred: libc::xucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::xucred>() as libc::socklen_t;
    const LOCAL_PEERCRED: libc::c_int = 0x001;
    let rc = unsafe {
        libc::getsockopt(fd, 0 /* SOL_LOCAL */, LOCAL_PEERCRED,
                         &mut cred as *mut _ as *mut _, &mut len)
    };
    if rc == 0 { Some(PeerInfo { pid: 0, uid: cred.cr_uid }) } else { None }
}

#[cfg(not(unix))]
fn peer_info(_stream: &tokio::net::UnixStream) -> Option<PeerInfo> { None }
```

- [ ] **Step 4: Run test — expect pass**

Run: `cargo test -p mur-agent-runtime --test transport_unix`

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/transport/unix_socket.rs mur-agent-runtime/tests/transport_unix.rs
git commit -m "feat(agent-runtime): Unix socket transport with SO_PEERCRED"
```

---

## Task 14: MCP client — spawn + initialize handshake

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/protocol/mcp_client.rs`
- Create: `/Users/david/Projects/mur/mur-agent-runtime/tests/fixtures/mock_mcp/main.rs` (new cargo bin)
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/mcp_client.rs`

- [ ] **Step 1: Create the mock MCP binary**

Create `mur-agent-runtime/tests/fixtures/mock_mcp/Cargo.toml`:

```toml
[package]
name = "mock_mcp"
version = "0.0.1"
edition = "2024"
[[bin]]
name = "mock_mcp"
path = "main.rs"
[dependencies]
serde_json = "1"
```

`main.rs`:

```rust
use std::io::{self, BufRead, Write};
fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => return };
        let req: serde_json::Value = match serde_json::from_str(&line) { Ok(v) => v, Err(_) => continue };
        let id = req["id"].clone();
        let method = req["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => serde_json::json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"mock_mcp","version":"0.0.1"}}),
            "tools/list" => serde_json::json!({"tools":[{"name":"echo","description":"echoes","inputSchema":{"type":"object"}}]}),
            "tools/call" => {
                let args = req["params"]["arguments"].clone();
                serde_json::json!({"content":[{"type":"text","text": format!("echo: {args}")}]})
            }
            _ => serde_json::json!(null),
        };
        let resp = serde_json::json!({"jsonrpc":"2.0","id":id,"result":result});
        writeln!(stdout, "{}", resp).unwrap();
        stdout.flush().unwrap();
    }
}
```

Register the mock in the main workspace `Cargo.toml` so tests can find the built binary:

```toml
[workspace]
members = ["mur-common", "mur-core", "mur-agent-runtime", "mur-agent-runtime/tests/fixtures/mock_mcp"]
```

- [ ] **Step 2: Write failing test**

`tests/mcp_client.rs`:

```rust
use mur_agent_runtime::protocol::mcp_client::McpClient;
use mur_common::agent::McpServerEntry;

#[tokio::test]
async fn initialize_and_tools_list() {
    let bin = env!("CARGO_BIN_EXE_mock_mcp");
    let entry = McpServerEntry { name: "mock".into(), command: bin.into(), args: vec![] };
    let mut client = McpClient::spawn(&entry).await.expect("spawn");
    let info = client.initialize().await.expect("init");
    assert_eq!(info.server_name, "mock_mcp");
    let tools = client.list_tools().await.expect("list");
    assert!(tools.iter().any(|t| t.name == "echo"));
    client.shutdown().await;
}
```

- [ ] **Step 3: Run — expect compile fail**

Run: `cargo test -p mur-agent-runtime --test mcp_client`
Expected: compile failure.

- [ ] **Step 4: Implement `protocol/mcp_client.rs`**

```rust
//! MCP client — subprocess stdio JSON-RPC 2.0.
use mur_common::agent::McpServerEntry;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

pub struct McpClient {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<Lines<BufReader<ChildStdout>>>,
    next_id: Mutex<u64>,
    pub server_name: String,
}

#[derive(Debug)]
pub struct InitializeInfo {
    pub server_name: String,
    pub server_version: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("json: {0}")] Json(#[from] serde_json::Error),
    #[error("mcp server closed stdout")] StreamClosed,
    #[error("mcp error: {0}")] Server(String),
}

impl McpClient {
    pub async fn spawn(entry: &McpServerEntry) -> Result<Self, McpError> {
        let mut child = Command::new(&entry.command)
            .args(&entry.args)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or(McpError::StreamClosed)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(McpError::StreamClosed)?).lines();
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            next_id: Mutex::new(1),
            server_name: entry.name.clone(),
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = { let mut g = self.next_id.lock().await; let v = *g; *g += 1; v };
        let req = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let line = format!("{req}\n");
        { let mut s = self.stdin.lock().await; s.write_all(line.as_bytes()).await?; s.flush().await?; }
        let mut stdout = self.stdout.lock().await;
        loop {
            let next = stdout.next_line().await?;
            let line = next.ok_or(McpError::StreamClosed)?;
            let v: Value = serde_json::from_str(&line)?;
            if v.get("id") == Some(&json!(id)) {
                if let Some(err) = v.get("error") {
                    return Err(McpError::Server(err.to_string()));
                }
                return Ok(v.get("result").cloned().unwrap_or(json!(null)));
            }
            // notifications (no matching id) — ignore for now
        }
    }

    pub async fn initialize(&mut self) -> Result<InitializeInfo, McpError> {
        let res = self.request("initialize", json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "mur-agent-runtime", "version": "0.1.0"}
        })).await?;
        Ok(InitializeInfo {
            server_name: res["serverInfo"]["name"].as_str().unwrap_or_default().to_string(),
            server_version: res["serverInfo"]["version"].as_str().unwrap_or_default().to_string(),
            protocol_version: res["protocolVersion"].as_str().unwrap_or_default().to_string(),
        })
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolInfo>, McpError> {
        let res = self.request("tools/list", json!({})).await?;
        let tools = res["tools"].as_array().cloned().unwrap_or_default();
        Ok(tools.into_iter().map(|t| ToolInfo {
            name: t["name"].as_str().unwrap_or_default().to_string(),
            description: t["description"].as_str().unwrap_or_default().to_string(),
            input_schema: t["inputSchema"].clone(),
        }).collect())
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        self.request("tools/call", json!({"name": name, "arguments": arguments})).await
    }

    pub async fn shutdown(self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}
```

- [ ] **Step 5: Run — expect pass**

Run: `cargo test -p mur-agent-runtime --test mcp_client`
Expected: test passes after Cargo builds mock_mcp.

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/protocol/mcp_client.rs mur-agent-runtime/tests/mcp_client.rs mur-agent-runtime/tests/fixtures/mock_mcp/ Cargo.toml
git commit -m "feat(agent-runtime): MCP client with spawn + initialize + tools/list + tools/call"
```

---

## Task 15: LLM client trait + Ollama provider

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/llm/mod.rs`
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/llm/ollama.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/llm_ollama.rs`

- [ ] **Step 1: Write failing test (uses httpmock)**

Add dev dep: `httpmock = "0.7"` to `mur-agent-runtime/Cargo.toml`.

```rust
// tests/llm_ollama.rs
use mur_agent_runtime::llm::{LlmClient, LlmRequest, LlmMessage};
use mur_agent_runtime::llm::ollama::OllamaClient;
use httpmock::prelude::*;
use serde_json::json;

#[tokio::test]
async fn ollama_generate_returns_text_and_usage() {
    let server = MockServer::start_async().await;
    let _mock = server.mock_async(|when, then| {
        when.method(POST).path("/api/chat");
        then.status(200).header("content-type", "application/json").json_body(json!({
            "model": "llama3.2",
            "message": {"role":"assistant","content":"Hello back"},
            "prompt_eval_count": 10,
            "eval_count": 5,
            "done": true
        }));
    }).await;
    let client = OllamaClient::new(server.base_url(), "llama3.2".into());
    let resp = client.generate(LlmRequest {
        messages: vec![LlmMessage { role: "user".into(), content: "Hi".into() }],
        temperature: Some(0.2), max_tokens: Some(100),
    }).await.unwrap();
    assert_eq!(resp.text, "Hello back");
    assert_eq!(resp.input_tokens, 10);
    assert_eq!(resp.output_tokens, 5);
}
```

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p mur-agent-runtime --test llm_ollama`

- [ ] **Step 3: Implement `llm/mod.rs` and `llm/ollama.rs`**

`llm/mod.rs`:

```rust
//! LLM client abstraction.
use async_trait::async_trait;
pub mod ollama;

#[derive(Debug, Clone)]
pub struct LlmMessage { pub role: String, pub content: String }

#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub text: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("http: {0}")] Http(String),
    #[error("rate limit")] RateLimit,
    #[error("timeout")] Timeout,
    #[error("invalid response: {0}")] InvalidResponse(String),
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;
    fn model_name(&self) -> &str;
}
```

`llm/ollama.rs`:

```rust
use super::{LlmClient, LlmError, LlmMessage, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::json;

pub struct OllamaClient {
    base_url: String,
    model: String,
    http: reqwest::Client,
}

impl OllamaClient {
    pub fn new(base_url: String, model: String) -> Self {
        Self { base_url, model, http: reqwest::Client::new() }
    }
}

#[async_trait]
impl LlmClient for OllamaClient {
    fn model_name(&self) -> &str { &self.model }

    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!("{}/api/chat", self.base_url);
        let messages: Vec<_> = req.messages.iter()
            .map(|m| json!({"role": m.role, "content": m.content})).collect();
        let mut body = json!({"model": self.model, "messages": messages, "stream": false});
        if let Some(t) = req.temperature { body["options"]["temperature"] = json!(t); }
        if let Some(m) = req.max_tokens { body["options"]["num_predict"] = json!(m); }
        let resp = self.http.post(url).json(&body).send().await
            .map_err(|e| LlmError::Http(e.to_string()))?;
        if resp.status() == 429 { return Err(LlmError::RateLimit); }
        let v: serde_json::Value = resp.json().await.map_err(|e| LlmError::Http(e.to_string()))?;
        let text = v["message"]["content"].as_str()
            .ok_or_else(|| LlmError::InvalidResponse("missing message.content".into()))?.to_string();
        let input_tokens = v["prompt_eval_count"].as_u64().unwrap_or(0);
        let output_tokens = v["eval_count"].as_u64().unwrap_or(0);
        Ok(LlmResponse { text, input_tokens, output_tokens, model: self.model.clone() })
    }
}
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p mur-agent-runtime --test llm_ollama`

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/llm/ mur-agent-runtime/tests/llm_ollama.rs mur-agent-runtime/Cargo.toml
git commit -m "feat(agent-runtime): LlmClient trait and Ollama provider"
```

---

## Task 16: Retry policy

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/retry.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/retry.rs`

- [ ] **Step 1: Write failing tests**

```rust
// tests/retry.rs
use mur_agent_runtime::retry::{run_with_retry, BackoffStrategy, Classifier};
use mur_common::RetryPolicy;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn succeeds_after_two_retries() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let policy = RetryPolicy {
        max_retries: 3, backoff: mur_common::agent::BackoffStrategy::Exponential,
        initial_delay_ms: 10, max_delay_ms: Some(100),
        retry_on: vec!["transient".into()],
    };
    let classifier: Classifier<()> = Box::new(|_| "transient");
    let result = run_with_retry(&policy, classifier, || {
        let a = a.clone();
        async move {
            let n = a.fetch_add(1, Ordering::SeqCst) + 1;
            if n < 3 { Err(()) } else { Ok("done") }
        }
    }).await;
    assert_eq!(result.unwrap(), "done");
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let policy = RetryPolicy {
        max_retries: 2, backoff: mur_common::agent::BackoffStrategy::Fixed,
        initial_delay_ms: 5, max_delay_ms: None,
        retry_on: vec!["x".into()],
    };
    let classifier: Classifier<()> = Box::new(|_| "x");
    let res: Result<&'static str, ()> = run_with_retry(&policy, classifier, || async { Err(()) }).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn unmatched_kind_does_not_retry() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = attempts.clone();
    let policy = RetryPolicy {
        max_retries: 3, backoff: mur_common::agent::BackoffStrategy::Fixed,
        initial_delay_ms: 1, max_delay_ms: None,
        retry_on: vec!["rate_limit".into()],
    };
    let classifier: Classifier<()> = Box::new(|_| "auth_error");
    let res: Result<&'static str, ()> = run_with_retry(&policy, classifier, || {
        let a = a.clone();
        async move { a.fetch_add(1, Ordering::SeqCst); Err(()) }
    }).await;
    assert!(res.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "must not retry non-matching error");
}
```

- [ ] **Step 2: Run — expect fail**

- [ ] **Step 3: Implement `retry.rs`**

```rust
//! Retry policy executor.
use mur_common::RetryPolicy;
use mur_common::agent::BackoffStrategy;
use std::future::Future;

pub type Classifier<E> = Box<dyn Fn(&E) -> &'static str + Send + Sync>;

pub async fn run_with_retry<T, E, F, Fut>(
    policy: &RetryPolicy,
    classifier: Classifier<E>,
    mut op: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                attempt += 1;
                let kind = classifier(&e);
                if attempt > policy.max_retries || !policy.retry_on.iter().any(|k| k == kind) {
                    return Err(e);
                }
                let delay = compute_delay(policy, attempt);
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn compute_delay(policy: &RetryPolicy, attempt: u32) -> std::time::Duration {
    let base = policy.initial_delay_ms;
    let cap = policy.max_delay_ms.unwrap_or(u64::MAX);
    let ms = match policy.backoff {
        BackoffStrategy::Fixed => base,
        BackoffStrategy::Linear => base.saturating_mul(attempt as u64),
        BackoffStrategy::Exponential => base.saturating_mul(1u64 << (attempt - 1).min(20)),
    }.min(cap);
    std::time::Duration::from_millis(ms)
}
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p mur-agent-runtime --test retry`

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/retry.rs mur-agent-runtime/tests/retry.rs
git commit -m "feat(agent-runtime): retry policy with linear/exp/fixed backoff"
```

---

## Task 17: Communication policy (receiver-authoritative)

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/communication_policy.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/comm_policy.rs`

- [ ] **Step 1: Write failing tests**

```rust
// tests/comm_policy.rs
use mur_agent_runtime::communication_policy::{sends_to_allows, accepts_from_allows, resolve_caller_name};

#[test]
fn sends_to_wildcard_allows_all() {
    assert!(sends_to_allows(&["*".into()], "anyone"));
}

#[test]
fn sends_to_empty_denies() {
    assert!(!sends_to_allows(&[], "anyone"));
}

#[test]
fn accepts_from_glob_matches() {
    let list = vec!["notify_*".into(), "watcher".into()];
    assert!(accepts_from_allows(&list, "notify_a"));
    assert!(accepts_from_allows(&list, "watcher"));
    assert!(!accepts_from_allows(&list, "stranger"));
}
```

(`resolve_caller_name` is stubbed for P0a — returns None unless a pid→name reverse lookup succeeds. The test for that is an integration-level test in Task 40.)

- [ ] **Step 2: Run — expect fail**

- [ ] **Step 3: Implement `communication_policy.rs`**

```rust
//! sends_to (intent filter) and accepts_from (authoritative security boundary).

use std::path::Path;

pub fn sends_to_allows(list: &[String], peer: &str) -> bool {
    list.iter().any(|p| glob_match(p, peer))
}

pub fn accepts_from_allows(list: &[String], caller: &str) -> bool {
    list.iter().any(|p| glob_match(p, caller))
}

fn glob_match(pattern: &str, s: &str) -> bool {
    if pattern == "*" { return true; }
    match glob::Pattern::new(pattern) {
        Ok(p) => p.matches(s),
        Err(_) => pattern == s,
    }
}

/// Given an agents directory, return the name whose running.lock's pid matches.
/// Returns None if not found (common for CLI callers — treat as trusted user).
pub fn resolve_caller_name(agents_dir: &Path, caller_pid: u32) -> Option<String> {
    let entries = std::fs::read_dir(agents_dir).ok()?;
    for entry in entries.flatten() {
        let lock_path = entry.path().join("running.lock");
        if !lock_path.exists() { continue; }
        let bytes = std::fs::read(&lock_path).ok()?;
        let lock: mur_common::LockFile = serde_json::from_slice(&bytes).ok()?;
        if lock.pid == caller_pid {
            return Some(lock.name);
        }
    }
    None
}
```

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p mur-agent-runtime --test comm_policy`

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/communication_policy.rs mur-agent-runtime/tests/comm_policy.rs
git commit -m "feat(agent-runtime): sends_to intent filter and accepts_from receiver verify"
```

---

## Task 18: Supervisor — startup sequence

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/supervisor.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/supervisor_startup.rs`

- [ ] **Step 1: Write failing integration test**

```rust
// tests/supervisor_startup.rs
use tempfile::TempDir;
use std::process::Stdio;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

#[tokio::test]
async fn runtime_starts_and_responds_to_agent_card_over_stdio() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("agent_t");
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::write(agent_home.join("profile.yaml"),
        include_str!("fixtures/profile_stdio.yaml")).unwrap();
    std::fs::write(agent_home.join("sys_prompt.md"), "You are a test.").unwrap();

    let bin = env!("CARGO_BIN_EXE_mur-agent-runtime");
    let mut child = Command::new(bin)
        .env("MUR_HOME", tmp.path())
        .args(["--profile", "agent_t", "start"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout).lines();

    use tokio::io::AsyncWriteExt;
    stdin.write_all(br#"{"jsonrpc":"2.0","id":1,"method":"agent/card"}
"#).await.unwrap();

    let first_line = tokio::time::timeout(std::time::Duration::from_secs(5), reader.next_line()).await.unwrap().unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_str(&first_line).unwrap();
    assert_eq!(resp["result"]["name"], "agent_t");

    // Send SIGTERM to shut down
    #[cfg(unix)] unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM); }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
}
```

Create `tests/fixtures/profile_stdio.yaml` — same as `profile_minimal.yaml` but with `name: agent_t`.

- [ ] **Step 2: Run — expect fail**

Run: `cargo test -p mur-agent-runtime --test supervisor_startup`
Expected: binary exits immediately (main.rs is stub).

- [ ] **Step 3: Implement `supervisor.rs`**

```rust
//! Agent runtime entrypoint — assembles profile, dispatcher, telemetry, and
//! drives the stdio (and optionally Unix-socket) transports until SIGTERM.

use crate::multi_call::{extract_profile_name, verify_name_match, DispatchError};
use crate::profile::Profile;
use crate::entitlements::detect_warnings;
use crate::telemetry_writer::{TelemetryWriter, Event};
use crate::lock_file::{LockHandle, write_lock};
use crate::socket_path::resolve_bind_target;
use crate::protocol::a2a_server::Dispatcher;
use crate::protocol::methods::{card::CardHandler, message_send::MessageSendHandler,
                                tasks::{TasksGetHandler, TasksCancelHandler, TasksListHandler}};
use crate::task_runner::TaskRunner;
use crate::transport::{stdio::serve_stdio, unix_socket::serve_unix};
use mur_common::{agent::LockTransports, LockFile};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{info, warn};

pub async fn entrypoint() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();

    // 1. Determine profile name from argv[0] (or --profile)
    let argv0 = std::env::args().next().unwrap_or_default();
    let name = match extract_profile_name(&argv0) {
        Ok(n) => n,
        Err(DispatchError::BareRuntime) => read_flag_profile_from_args()?,
        Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
    };

    // 2. Resolve agent_home and load profile
    let mur_home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().expect("no home").join(".mur"));
    let agent_home = mur_home.join("agents").join(&name);
    let profile = match Profile::load(&agent_home) {
        Ok(p) => p,
        Err(e) => { eprintln!("error[profile_invalid]: {e}"); std::process::exit(1); }
    };
    if let Err(e) = verify_name_match(&name, &profile.inner.name) {
        eprintln!("error: {e}"); std::process::exit(1);
    }

    // 3. Warn on loose entitlements
    for w in detect_warnings(&profile.inner) {
        warn!(kind = ?w.kind, "{}", w.message);
    }

    // 4. Spawn telemetry writer
    let (writer, notif_rx) = TelemetryWriter::new(
        agent_home.join("telemetry"),
        profile.inner.name.clone(),
        profile.inner.id.clone(),
    ).await?;

    // 5. Acquire running.lock
    let lock_path = agent_home.join("running.lock");
    let _lock_handle = crate::lock_file::LockHandle::acquire(&lock_path)
        .map_err(|e| anyhow::anyhow!("already running ({e})"))?;

    // 6. Build task runner + dispatcher
    let profile_arc = Arc::new(profile.clone());
    let runner = Arc::new(TaskRunner::new_stub_echo());   // Task 19 swaps to real backend
    let mut dispatcher = Dispatcher::new();
    dispatcher.register("agent/card", Box::new(CardHandler::new(profile_arc.clone())));
    dispatcher.register("message/send", Box::new(MessageSendHandler::new(runner.clone())));
    dispatcher.register("tasks/get", Box::new(TasksGetHandler::new(runner.clone())));
    dispatcher.register("tasks/cancel", Box::new(TasksCancelHandler::new(runner.clone())));
    dispatcher.register("tasks/list", Box::new(TasksListHandler::new(runner.clone())));

    // 7. Transports
    let mut transport_tasks = vec![];
    let mut lock_transports = LockTransports { stdio: profile.inner.transport.stdio, unix_socket: None, tcp: None };

    if profile.inner.transport.socket.enabled
        && profile.inner.transport.socket.bind.starts_with("unix://")
    {
        let canonical = PathBuf::from(profile.inner.transport.socket.bind.trim_start_matches("unix://"));
        let res = resolve_bind_target(&canonical, &profile.inner.id)?;
        lock_transports.unix_socket = Some(canonical.to_string_lossy().to_string());
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        // Fan telemetry → transport notification channel
        let mut notif = notif_rx;                       // one-owner; we'll move into spawn
        let txc = tx.clone();
        transport_tasks.push(tokio::spawn(async move {
            while let Some(n) = notif.recv().await {
                let _ = txc.send(n).await;
            }
        }));
        let dispatcher_clone = dispatcher.clone_structure();   // see dispatcher note below
        let bind = res.bind_path.clone();
        transport_tasks.push(tokio::spawn(async move {
            let _ = serve_unix(dispatcher_clone, bind, rx).await;
        }));
    }

    // Stdio — always last so telemetry_rx is consumed by it if no socket
    if profile.inner.transport.stdio {
        let (_, dummy_rx) = tokio::sync::mpsc::channel(1);
        transport_tasks.push(tokio::spawn(async move {
            let _ = serve_stdio(dispatcher, tokio::io::stdin(), tokio::io::stdout(), dummy_rx).await;
        }));
    }

    // 8. Write running.lock
    let lock = LockFile {
        schema: 1,
        uuid: profile.inner.id.clone(),
        name: profile.inner.name.clone(),
        pid: std::process::id(),
        ppid: parent_pid(),
        started_at: chrono::Utc::now().to_rfc3339(),
        binary_version: format!("mur-agent-runtime {}", env!("CARGO_PKG_VERSION")),
        transports: lock_transports,
        card_digest: profile.digest.clone(),
        capabilities: profile.inner.capabilities.clone(),
    };
    write_lock(&lock_path, &lock)?;
    info!("agent {} ({}) ready", profile.inner.name, profile.inner.id);
    writer.emit(Event::Heartbeat { uptime_s: 0, mem_mb: 0, active_tasks: 0 }).await;

    // 9. Wait for SIGTERM / SIGINT
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = sigterm.recv() => info!("SIGTERM received"),
        _ = sigint.recv() => info!("SIGINT received"),
    }
    // 10. Graceful shutdown (implement in Task 21)
    for t in transport_tasks { t.abort(); }
    Ok(())
}

fn parent_pid() -> u32 {
    #[cfg(unix)] unsafe { libc::getppid() as u32 }
    #[cfg(not(unix))] 0
}

fn read_flag_profile_from_args() -> anyhow::Result<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--profile" {
            if let Some(name) = args.next() { return Ok(name); }
        }
        if let Some(n) = a.strip_prefix("--profile=") { return Ok(n.to_string()); }
    }
    anyhow::bail!("bare mur-agent-runtime requires --profile <name>")
}
```

Note on `dispatcher.clone_structure()`: Dispatcher's HashMap holds `Box<dyn MethodHandler>`; trait objects aren't Clone. For the transport to share dispatcher between stdio and Unix socket, either (a) wrap Dispatcher in `Arc`, or (b) build two separate dispatchers with identical registrations. The simpler fix is (a) — change Dispatcher's field to `Arc<HashMap<String, Arc<dyn MethodHandler>>>` and have `dispatch` take `&self`. Update Task 8's code accordingly in this task's commit.

Add `dirs = "5"` to dependencies.

- [ ] **Step 4: Run — expect pass**

Run: `cargo test -p mur-agent-runtime --test supervisor_startup`
Expected: `agent_t` responds with its Agent Card over stdio then shuts down on SIGTERM.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs mur-agent-runtime/src/protocol/a2a_server.rs mur-agent-runtime/src/main.rs mur-agent-runtime/tests/supervisor_startup.rs mur-agent-runtime/tests/fixtures/profile_stdio.yaml mur-agent-runtime/Cargo.toml
git commit -m "feat(agent-runtime): supervisor startup sequence with stdio + Unix socket"
```

---

## Task 19: Supervisor graceful shutdown + cancellation

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/supervisor.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/supervisor_shutdown.rs`

- [ ] **Step 1: Write failing test** — after sending SIGTERM, running.lock must be removed, telemetry flushed (JSONL file has shutdown event). Essentially assert that `lock_path.exists()` is false 3 seconds after SIGTERM.

- [ ] **Step 2: Run — expect failure** (Task 18 aborts transports but does not delete lock).

- [ ] **Step 3: Implement shutdown**

Replace step 10 of Task 18 with:

```rust
    // 10. Graceful shutdown
    info!("begin graceful shutdown");
    let deadline = std::time::Duration::from_secs(profile.inner.lifecycle.stop_timeout_secs);
    // Cancel active tasks
    // (TaskRunner future work: snapshot all active task ids and send cancel to each)
    let shutdown_start = std::time::Instant::now();
    while shutdown_start.elapsed() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // runner.has_active().await => break if none
        break;
    }
    for t in transport_tasks { t.abort(); }
    writer.emit(Event::Warning { kind: "shutdown".into(), message: "SIGTERM".into() }).await;
    writer.flush().await;
    let _ = std::fs::remove_file(&lock_path);
    Ok(())
```

- [ ] **Step 4: Run — expect pass**

- [ ] **Step 5: Commit**

```bash
git commit -am "feat(agent-runtime): graceful shutdown removes lock and flushes telemetry"
```

---

## Task 20: task/progress notifications

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/task_runner.rs`
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/protocol/methods/message_send.rs`
- Test: add progress assertions to `tests/methods.rs`

- [ ] **Step 1: Modify MessageSendHandler to accept a progress sink** (`mpsc::Sender<Event>`).
- [ ] **Step 2: Emit `Event::TaskProgress { stage: "llm_reasoning"|"tool_call"|"synthesis", task_id, percent }` during `run_sync`.**
- [ ] **Step 3: Test that during a `message/send` a progress notification lands on the telemetry channel before the response.**
- [ ] **Step 4: Commit `feat(agent-runtime): task/progress notifications during sync tasks`.**

---

## Task 21: Wire Ollama backend into TaskRunner

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/task_runner.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/task_runner_ollama.rs`

- [ ] **Step 1: Add `RunnerBackend::Llm(Arc<dyn LlmClient>)` variant**; `run_sync` calls `client.generate(...)`, emits `Event::LlmCall` telemetry, returns the assistant message.
- [ ] **Step 2: Test with httpmock Ollama** — verify LLM call telemetry fires and task completes.
- [ ] **Step 3: Commit.**

---

## Task 22: `mur agent create` (mur-core cmd/agent.rs, interactive + non-interactive)

**Files:**
- Create: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/mod.rs` (register `pub mod agent;`)
- Modify: `/Users/david/Projects/mur/mur-core/src/main.rs` (add `Agent(...)` subcommand)
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_create.rs`

- [ ] **Step 1: Write failing E2E**: run `mur agent create agent_x --no-interactive --display-name=X --model=llama3.2:3b` under `MUR_HOME=<tmp>`. Assert:
  - `<tmp>/agents/agent_x/profile.yaml` exists and parses into `AgentProfile` with `name=agent_x` and `persona.category=custom`.
  - `<tmp>/agents/agent_x/sys_prompt.md` exists.
  - `<install_dir>/mur_agent_agent_x` symlink points to `mur-agent-runtime` (override install dir via `MUR_AGENT_BIN_DIR=<tmp2>`).
  - `agent_x` has a valid UUIDv7.

- [ ] **Step 2: Implement**:
  - `dialoguer` prompts for interactive mode; `clap` for non-interactive.
  - Generate UUIDv7 via `Uuid::now_v7()`.
  - Use `entitlements::preset_for_category` to seed the entitlements block.
  - Write profile via `serde_yaml_ng::to_string(&AgentProfile)` then prepend a header comment.
  - Create symlink via `std::os::unix::fs::symlink` / `std::fs::hard_link` on Windows.
  - Respect `MUR_AGENT_BIN_DIR` (env) / `~/.local/bin` / same-dir-as-runtime / `~/.mur/bin` ordering (Task 22.1 sub-step inlined).
- [ ] **Step 3-5: Run test, pass, commit** `feat(core): mur agent create`.

---

## Task 23: `mur agent list` + `mur agent status`

**Files:**
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_list_status.rs`

- [ ] **Step 1: Failing test**: create two agents (`--no-interactive`), start one via `Command::new("mur-agent-runtime")` with env `MUR_HOME`, then run `mur agent list --json` and verify the JSON has two entries, exactly one with `status:"running"`.
- [ ] **Step 2: Implement `list`**: walk `<mur_home>/agents/*/running.lock`; read each; use `lock_file::is_stale`; assemble 7-column table (spec §9.2 — NAME/STATUS/UPTIME/PID/TASKS/MEM/CATEGORY). For TASKS/MEM, try `proc` querying (Linux) / `libproc` (macOS); fall back to `-` if unavailable.
- [ ] **Step 3: Implement `status <name>`** per §9.2 — systemctl-style output.
- [ ] **Step 4: Run + pass + commit** `feat(core): mur agent list and status`.

---

## Task 24: `mur agent stop` + `remove` + `rename`

**Files:**
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_lifecycle.rs`

- [ ] **Step 1: Failing tests**: start → `mur agent stop name` → `is_stale` true; `mur agent remove name` removes symlink, keeps dir; `--purge` removes dir; `mur agent rename old new` renames dir, updates profile.name, updates symlink; running agent during rename → error.
- [ ] **Step 2: Implement** each subcommand; use SIGTERM + wait up to `lifecycle.stop_timeout_secs`, then SIGKILL.
- [ ] **Step 3-5: Pass + commit**.

---

## Task 25: `mur agent send` + `card` CLI forwarders

**Files:**
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_send.rs`

- [ ] **Step 1: Failing test**: start a running agent; `mur agent send name '{"role":"user","parts":[{"kind":"text","text":"hi"}]}'` returns the task's JSON result on stdout; exit 0.
- [ ] **Step 2: Implement**: read `running.lock`, pick transport (prefer Unix socket), open connection, send `message/send`, print response. `mur agent card <name>` dials `agent/card` analogously.
- [ ] **Step 3: Pass + commit**.

---

## Task 26: `mur agent install-service` (launchd / systemd generator)

- [ ] **Step 1: Failing test**: run `mur agent install-service agent_x --dry-run` on macOS/Linux. On macOS assert output contains a valid `<plist>` referring to `mur_agent_agent_x start`. On Linux assert output has `[Unit]`, `[Service]`, `ExecStart=...mur_agent_agent_x start`.
- [ ] **Step 2: Implement** platform-specific template generation. `--dry-run` prints; without `--dry-run` writes to `~/Library/LaunchAgents/run.mur.agent.<name>.plist` or `~/.config/systemd/user/mur-agent-<name>.service` and invokes `launchctl load` / `systemctl --user daemon-reload && systemctl --user enable --now <unit>`.
- [ ] **Step 3: Pass + commit**.

---

## Task 27: `mur agent prompt {show,edit,set}`

**Files:**
- Create: `/Users/david/Projects/mur/mur-core/src/cmd/agent_prompt.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_prompt.rs`

- [ ] **Step 1: Failing test**: `prompt show name` prints the sys_prompt.md contents; `prompt set name "hello"` writes exactly "hello"; `prompt set name -f path` reads from file; backup `.prompt.md.bak` is created before overwrite.
- [ ] **Step 2: Implement** via simple `fs::read_to_string` / `fs::write` with atomic temp-rename and `.bak` preservation; `edit` spawns `$EDITOR` (default `vi`) on the file.
- [ ] **Step 3: Pass + commit**.

---

## Task 28: `mur agent mcp {list,add,remove,rename}` (with spawn allowlist sync)

**Files:**
- Create: `/Users/david/Projects/mur/mur-core/src/cmd/agent_mcp.rs`
- Create: `/Users/david/Projects/mur/mur-core/src/cmd/yaml_edit.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_mcp.rs`

- [ ] **Step 1: Failing test**:
  - After `mur agent mcp add name pw npx -- -y @playwright/mcp@latest`:
    - `profile.mcp_servers` has a new entry `name=pw, command=npx, args=["-y","@playwright/mcp@latest"]`
    - `entitlements.processes.spawn.allowed` now contains `"npx"`
    - file `.profile.yaml.bak` exists and is the pre-edit contents
    - comments in the original YAML are preserved (write a profile with `# preserved` comment and assert it's still there)
- [ ] **Step 2: Implement `yaml_edit.rs`** — use `serde_yaml_ng`'s round-trip (preserves comments per `0.10+`); wrap mutation in atomic write: write to `.profile.yaml.tmp`, fsync, rename; copy previous to `.profile.yaml.bak`.
- [ ] **Step 3: Implement `agent_mcp.rs`** subcommands — each mutates `AgentProfile` via yaml_edit; `add` also inserts basename of command into `entitlements.processes.spawn.allowed` if absent.
- [ ] **Step 4: Pass + commit**.

---

## Task 29: `mur agent skill {list,add,remove,show}`

**Files:**
- Create: `/Users/david/Projects/mur/mur-core/src/cmd/agent_skill.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_skill.rs`

- [ ] **Step 1: Failing test**: `skill add name path/to/skill.md` copies file into `agents/name/skills/`, appends `"skills/<basename>"` to profile.skills; `skill remove name skill` removes from list and deletes file if orphaned; `skill show name skill` prints file contents; `skill list name` prints IDs.
- [ ] **Step 2-5: Implement + test + commit**.

---

## Task 30: `mur agent perm ...` (all perm subcommands)

**Files:**
- Create: `/Users/david/Projects/mur/mur-core/src/cmd/agent_perm.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_perm.rs`

Subcommands (implement each with a failing test):
- `perm show name [section]`
- `perm set-mode name network.outbound <mode>`
- `perm allow-host name <glob>` / `perm deny-host name <glob>` / `perm list-hosts name`
- `perm allow-read name <path>` / `perm allow-write name <path>` / `perm deny-path name <path>`
- `perm allow-spawn name <binary>` / `perm deny-spawn name <binary>`
- `perm set-limit name <key> <value>` (keys: `memory_mb`, `file_descriptors`, `processes`)

Each mutation:
1. Load profile via yaml_edit.
2. Modify the correct field.
3. Re-validate the resulting AgentProfile against schema; reject invalid.
4. Write back atomically with `.bak`.
5. If agent is running (lock exists, not stale), print `warning: restart required for changes to take effect`.

- [ ] Commit `feat(core): mur agent perm subcommands`.

---

## Task 31: `yaml_edit.rs` hardening — atomic + comment preservation tests

**Files:**
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/yaml_edit.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/yaml_edit.rs`

- [ ] **Step 1: Failing tests**:
  - Mid-edit crash simulation (write `.tmp`, inject kill, re-open; original still valid).
  - Comments in original preserved across `load → mutate a scalar → save`.
- [ ] **Step 2: Ensure** `serde_yaml_ng::with_comments` or equivalent round-trip is used.
- [ ] **Step 3: Pass + commit**.

---

## Task 32: `mur agent export --format=pkg`

**Files:**
- Create: `/Users/david/Projects/mur/mur-agent-runtime/src/export/pkg.rs`
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs` (forward to runtime export module)
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/export_pkg.rs`

- [ ] **Step 1: Failing test**:
  - Export `agent_a` → produce `agent_a.murpkg`.
  - Open it as tar.gz; assert members: `manifest.yaml`, `profile.yaml`, `sys_prompt.md`, `skills/*.md`, `README.md`.
  - Assert `manifest.yaml.sanitized.removed_fields` includes any secret fields (e.g., `notifications[].webhook_url_env`).
- [ ] **Step 2: Implement `pkg.rs`**:
  - Use `tar::Builder<flate2::GzEncoder>`.
  - Run `sanitize_profile(&mut profile)` before packing (strip `webhook_url`, `token_file`, fields ending in `_env` that reference secrets; record keys).
  - Build `manifest.yaml` with schema `mur-agent-package/1`, `exported_at`, `original_uuid`, `prerequisites.mcp_servers`.
  - Generate README.md from template listing MCP prereqs.
- [ ] **Step 3: Pass + commit**.

---

## Task 33: `mur agent import <murpkg>`

**Files:**
- Create: `/Users/david/Projects/mur/mur-agent-runtime/src/import.rs`
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/import_pkg.rs`

- [ ] **Step 1: Failing tests**:
  - Export → import round-trip creates a *new* UUID but same name (unless `--as` given).
  - Missing MCP command in `PATH` → import prints a warning but still creates the agent.
- [ ] **Step 2: Implement**:
  - Unpack tar.gz into a temp dir.
  - Validate manifest schema + `min_runtime_version` compatibility.
  - Generate new UUIDv7; rewrite `profile.id` + `created_at` + `updated_at`.
  - Scan `prerequisites.mcp_servers[].command_basename` for `which` availability.
  - Move unpacked files to `<mur_home>/agents/<name>/`.
  - Create symlink.
- [ ] **Step 3: Pass + commit**.

---

## Task 34: Self-contained binary — `build.rs` asset embedding

**Files:**
- Create: `/Users/david/Projects/mur/mur-agent-runtime/build.rs`
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/export/bin_embed.rs`
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/Cargo.toml` (add `embedded-agent` feature)
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/bin_embed.rs`

- [ ] **Step 1: Failing integration test** (gated on `--features=embedded-agent`):
  - Set `MUR_EXPORT_AGENT_DIR` env to a fixture agent dir.
  - Build the runtime with `cargo build --features=embedded-agent`.
  - Run the resulting binary; it should print its embedded agent's Card when called with `card`.
  - Verify embedded digest in agent card matches the digest of the source profile.yaml.
- [ ] **Step 2: Implement `build.rs`**:
  - Read env var `MUR_EXPORT_AGENT_DIR` at compile time.
  - If set and feature `embedded-agent` is enabled: serialize agent dir to tar.gz in `OUT_DIR/embedded_agent.tar.gz`.
  - Emit `cargo:rustc-env=MUR_EMBEDDED_AGENT_PATH=<out>` so runtime can `include_bytes!` it at a known path.
- [ ] **Step 3: Implement `export/bin_embed.rs`**:
  - `#[cfg(feature="embedded-agent")] pub const EMBEDDED_TAR: &[u8] = include_bytes!(env!("MUR_EMBEDDED_AGENT_PATH"));`
  - Expose `has_embedded_agent()` and `embedded_manifest()`.
- [ ] **Step 4: Pass + commit**.

---

## Task 35: Self-contained binary — first-run asset extraction

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/export/extract.rs`
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/supervisor.rs` (detect embedded + divert agent_home)
- Test: `/Users/david/Projects/mur/mur-agent-runtime/tests/embed_extract.rs`

- [ ] **Step 1: Failing test**: run embedded binary in an empty `$HOME`; ~/.cache/murmur/<uuid>/ should populate with profile.yaml, sys_prompt.md, skills/; second run should not re-extract unless digest changes.
- [ ] **Step 2: Implement** `extract.rs`:
  - Compute target dir: `$XDG_CACHE_HOME/murmur/<uuid>/` (fallback `~/.cache/murmur/<uuid>`).
  - Compare `.extract_digest` marker against embedded digest.
  - If mismatch or missing, extract tar.gz via `tar::Archive<flate2::GzDecoder>`.
  - Set mode 0600 on the marker file.
- [ ] **Step 3: Modify supervisor entrypoint** — when `has_embedded_agent()` returns true, skip `MUR_HOME`-based discovery and use the extraction dir as `agent_home`. Respect `MUR_AGENT_EXTERNAL_PROFILE` override.
- [ ] **Step 4: Pass + commit**.

---

## Task 36: Self-contained binary — prerequisite check + `mur agent export --format=bin`

**Files:**
- Modify: `/Users/david/Projects/mur/mur-agent-runtime/src/export/prereq_check.rs`
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_export_bin.rs`

- [ ] **Step 1: Failing test**:
  - `mur agent export agent_a --format=bin -o /tmp/my_agent` produces an executable with `file` reporting "ELF" / "Mach-O" / "PE32+".
  - Running `/tmp/my_agent card` in a fresh tmpdir prints the card JSON containing `"name":"agent_a"`.
  - Removing `npx` from PATH and running `/tmp/my_agent start` prints `error: missing MCP prerequisite: npx — install with: ...`.
- [ ] **Step 2: Implement**:
  - `agent.rs`: when `--format=bin`, invoke `cargo build -p mur-agent-runtime --release --features=embedded-agent` with `MUR_EXPORT_AGENT_DIR=<agent_home>` and `CARGO_TARGET_DIR=<tmp>`; then copy the resulting binary to `-o`. If `--target=<triple>` is passed, add `--target` to cargo and `rustup target add` first if needed.
  - `prereq_check.rs`: on startup, if `has_embedded_agent()`, iterate `embedded_manifest().prerequisites.mcp_servers`, run `which` for each; if missing, print friendly error + `hint` and exit 2 before reaching normal startup.
- [ ] **Step 3: Pass + commit**.

---

## Task 37: `mur agent stats` + `mur agent logs`

**Files:**
- Modify: `/Users/david/Projects/mur/mur-core/src/cmd/agent.rs`
- Test: `/Users/david/Projects/mur/mur-core/tests/agent_stats.rs`

- [ ] **Step 1: Failing test**: after a running agent processes a task, `mur agent stats name` prints total LLM calls, tokens in/out, errors, avg latency; `mur agent logs name --tail 5` tails stderr.log.
- [ ] **Step 2: Implement**:
  - `stats`: read all `telemetry/*.jsonl` files, aggregate by event kind.
  - `logs`: `open + seek-end + read` the stderr.log; `--tail N` prints last N lines; `--follow` streams via `notify` crate.
- [ ] **Step 3: Pass + commit**.

---

## Task 38: `mur agent card` CLI (discover-and-print)

- [ ] Already partially covered in Task 25; explicit task here to unify `mur agent card name` to simply connect to the agent (or start it in card mode if not running) and print the Card JSON.
- [ ] Commit `feat(core): mur agent card forwards to running or ephemeral agent`.

---

## Task 39: E2E smoke-test suite + coverage gate

**Files:**
- Create: `/Users/david/Projects/mur/mur-agent-runtime/tests/e2e/` (multiple test files)
- Create: `/Users/david/Projects/mur/scripts/e2e/run-all.sh`
- Modify: `.github/workflows/ci.yml` (if present; else document manual run)

- [ ] **Step 1: Write E2E tests** listed in spec §12.4:
  - `e2e_create_and_launch.rs`
  - `e2e_roundtrip_send.rs`
  - `e2e_remove_purge.rs`
  - `e2e_argv0_spoofing.rs`
  - `e2e_list_filters.rs`
  - `e2e_export_import_murpkg.rs`
  - `e2e_export_bin_run_standalone.rs`
  - `e2e_mgmt_cli_suite.rs`
- [ ] **Step 2: Script `scripts/e2e/run-all.sh`**:
  - Set up isolated `MUR_HOME` in `mktemp -d`.
  - Run each `tests/e2e/*.rs` with `cargo test --test <name>`.
  - Exit 1 if any fail.
- [ ] **Step 3: Coverage gate**:
  - Add `cargo-llvm-cov` to dev setup.
  - `cargo llvm-cov --workspace --fail-under-lines 85` in CI for unit + lib.
- [ ] **Step 4: Commit** `test(agent-runtime): E2E suite and 85% coverage gate`.

---

## Task 40: Documentation + final polish

**Files:**
- Create: `/Users/david/Projects/mur/mur-agent-runtime/README.md`
- Modify: `/Users/david/Projects/mur/CLAUDE.md` (add Agent Runtime section)
- Modify: `/Users/david/Projects/mur/README.md` (brief mention)
- Create: `/Users/david/Projects/mur/docs/superpowers/plans/2026-04-22-murmur-p0a-agent-runtime-plan-COMPLETE.md` (short changelog)

- [ ] **Step 1**: README for the new crate — link to spec, show `mur agent create / start / card / list / export` walkthrough.
- [ ] **Step 2**: Add a "Agent Runtime (murmur)" subsection to project `CLAUDE.md` pointing to spec + plan.
- [ ] **Step 3**: Brief paragraph in top-level README.
- [ ] **Step 4**: Commit `docs(agent-runtime): README + CLAUDE.md + top-level README update`.

---

## Final verification

After all tasks:

- [ ] `cargo build --workspace --release` succeeds.
- [ ] `cargo test --workspace` green.
- [ ] `cargo clippy --workspace -- -D warnings` green.
- [ ] `cargo fmt --check` green.
- [ ] `cargo llvm-cov --workspace --fail-under-lines 85` green.
- [ ] `scripts/e2e/run-all.sh` exits 0 on macOS + Linux.
- [ ] Spec §2 goals G1-G9 each map to at least one green test.

All clean → P0a is done; proceed to writing the P0b sibling spec.
