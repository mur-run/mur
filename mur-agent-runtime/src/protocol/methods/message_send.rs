//! message/send handler.

use crate::protocol::a2a_server::{HandlerError, MethodHandler};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use crate::telemetry_writer::Event;
use async_trait::async_trait;
use mur_common::a2a::Message;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct MessageSendHandler {
    runner: Arc<TaskRunner>,
    progress: Option<mpsc::Sender<Event>>,
}

impl MessageSendHandler {
    pub fn new(runner: Arc<TaskRunner>) -> Self {
        Self {
            runner,
            progress: None,
        }
    }

    pub fn with_progress(runner: Arc<TaskRunner>, progress: mpsc::Sender<Event>) -> Self {
        Self {
            runner,
            progress: Some(progress),
        }
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
        let spec = TaskSpec {
            input: message,
            context_task_id,
        };

        self.emit_progress("pending", "llm_reasoning", None).await;
        let outcome = self.runner.run_sync(spec).await;
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
