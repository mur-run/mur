//! message/send handler.

use crate::protocol::a2a_server::{HandlerError, MethodHandler};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use crate::telemetry_writer::Event;
use async_trait::async_trait;
use mur_common::a2a::Message;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct MessageSendHandler {
    runner: Arc<TaskRunner>,
    progress: Option<mpsc::Sender<Event>>,
    /// Socket notification channel. When present, LLM token deltas are streamed
    /// to connected clients as `message/delta` notifications as they generate.
    notifier: Option<mpsc::Sender<Value>>,
}

impl MessageSendHandler {
    pub fn new(runner: Arc<TaskRunner>) -> Self {
        Self {
            runner,
            progress: None,
            notifier: None,
        }
    }

    pub fn with_progress(runner: Arc<TaskRunner>, progress: mpsc::Sender<Event>) -> Self {
        Self {
            runner,
            progress: Some(progress),
            notifier: None,
        }
    }

    /// Stream token deltas over `notifier` (the socket notification channel).
    pub fn with_streaming(mut self, notifier: mpsc::Sender<Value>) -> Self {
        self.notifier = Some(notifier);
        self
    }

    async fn emit_progress(&self, task_id: &str, stage: &str, percent: Option<u8>) {
        if let Some(tx) = &self.progress {
            let _ = tx
                .send(Event::TaskProgress {
                    task_id: task_id.to_string(),
                    stage: stage.to_string(),
                    message: None,
                    percent,
                })
                .await;
        }
    }
}

#[async_trait]
impl MethodHandler for MessageSendHandler {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let message: Message = serde_json::from_value(p["message"].clone())
            .map_err(|e| HandlerError::InvalidParams(format!("message: {e}")))?;
        let context_task_id = p
            .get("context")
            .and_then(|c| c.get("task_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        // Caller-supplied id for this turn (distinct from `context.task_id`,
        // which threads multi-turn context). When present the runner honors it
        // so the client can cancel by an id it already holds; absent → None,
        // back-compatible.
        let task_id = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let spec = TaskSpec {
            input: message,
            context_task_id,
            task_id,
        };

        self.emit_progress("pending", "llm_reasoning", None).await;
        let outcome = match &self.notifier {
            Some(notifier) => {
                // Forward each LLM token delta to the connected client as a
                // `message/delta` notification while the reply generates.
                let (delta_tx, mut delta_rx) = mpsc::channel::<crate::llm::StreamDelta>(256);
                let notifier = notifier.clone();
                let forward = tokio::spawn(async move {
                    while let Some(d) = delta_rx.recv().await {
                        let note = json!({
                            "jsonrpc": "2.0",
                            "method": "message/delta",
                            "params": { "text": d.text, "thinking": d.thinking },
                        });
                        if notifier.send(note).await.is_err() {
                            break;
                        }
                    }
                });
                let outcome = self.runner.run_sync_streaming(spec, delta_tx).await;
                let _ = forward.await;
                outcome
            }
            None => self.runner.run_sync(spec).await,
        };
        match outcome {
            TaskOutcome::Completed(task)
            | TaskOutcome::Failed(task)
            | TaskOutcome::Cancelled(task) => {
                self.emit_progress(&task.id, "synthesis", Some(100)).await;
                serde_json::to_value(&task).map_err(|e| HandlerError::Internal(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_runner::TaskRunner;

    fn user_params(task_id: Option<&str>) -> Value {
        let mut p = json!({
            "message": { "role": "user", "parts": [{ "kind": "text", "text": "hi" }] }
        });
        if let Some(id) = task_id {
            p["task_id"] = json!(id);
        }
        p
    }

    #[tokio::test]
    async fn supplied_task_id_flows_into_returned_task() {
        let handler = MessageSendHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let out = handler
            .handle(Some(user_params(Some("task-from-client"))))
            .await
            .expect("handle ok");
        assert_eq!(
            out.get("id").and_then(Value::as_str),
            Some("task-from-client")
        );
    }

    #[tokio::test]
    async fn absent_task_id_is_back_compatible() {
        let handler = MessageSendHandler::new(Arc::new(TaskRunner::new_stub_echo()));
        let out = handler
            .handle(Some(user_params(None)))
            .await
            .expect("handle ok");
        // Runner generated its own id (prefixed "task-"), not a client id.
        let id = out.get("id").and_then(Value::as_str).unwrap_or_default();
        assert!(id.starts_with("task-"), "generated id, got {id:?}");
        assert_ne!(id, "task-from-client");
    }
}
