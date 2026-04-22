//! message/send handler.

use crate::protocol::a2a_server::{HandlerError, MethodHandler};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use async_trait::async_trait;
use mur_common::a2a::Message;
use serde_json::Value;
use std::sync::Arc;

pub struct MessageSendHandler {
    runner: Arc<TaskRunner>,
}

impl MessageSendHandler {
    pub fn new(runner: Arc<TaskRunner>) -> Self {
        Self { runner }
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
        match self.runner.run_sync(spec).await {
            TaskOutcome::Completed(task)
            | TaskOutcome::Failed(task)
            | TaskOutcome::Cancelled(task) => {
                serde_json::to_value(&task).map_err(|e| HandlerError::Internal(e.to_string()))
            }
        }
    }
}
