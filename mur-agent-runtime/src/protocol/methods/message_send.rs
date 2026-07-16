//! message/send handler.

use crate::llm::RequestIntent;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use crate::telemetry_writer::Event;
use async_trait::async_trait;
use mur_common::a2a::Message;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Buffer of the LLM-token → notification forwarding channel. Bounded; overflow
/// is lossy (a dropped delta beats stalling generation on a slow client).
const STREAM_DELTA_CAP: usize = 256;

/// Buffer for per-turn steering messages (user interjections sent mid-loop).
const STEER_CAP: usize = 16;

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
    async fn handle(
        &self,
        params: Option<Value>,
        ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
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
        // Kept for stamping deltas and routing this turn's HITL prompts after
        // `spec` consumes the originals below.
        let turn_task_id = task_id.clone();
        let turn_context_id = context_task_id.clone();
        let output_artifact_path = p
            .get("output_artifact_path")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let spec = TaskSpec {
            input: message,
            context_task_id,
            task_id,
            // Direct message/send carries no fleet context — only channel/delegate
            // does, so fleet-scoped skills stay hidden on this path (fail-closed).
            active_fleet: None,
            active_team: None,
            // A live client is synchronously waiting on this reply — always
            // Interactive, never eligible for Smart cheap-model downgrade.
            intent: RequestIntent::Interactive,
            output_artifact_path,
        };

        self.emit_progress("pending", "llm_reasoning", None).await;
        // Prefer the issuing connection's per-request sink (so deltas/HITL reach
        // ONLY this client); fall back to any baked-in notifier for transports
        // that don't route per-connection.
        let stream_notifier = ctx.notifier.clone().or_else(|| self.notifier.clone());
        let outcome = match stream_notifier {
            Some(notifier) => {
                // Route this turn's HITL approval prompts back to this same
                // connection (looked up by task id inside the runner).
                if let Some(tid) = &turn_task_id {
                    self.runner
                        .register_client_notifier(tid, notifier.clone())
                        .await;
                }
                // Steering channel: only when this turn has a task id to address
                // it by. Without an id the sender would be immediately dropped,
                // leaving the agentic loop with a permanently-closed receiver.
                let steer_rx = if let Some(tid) = &turn_task_id {
                    let (steer_tx, steer_rx) = tokio::sync::mpsc::channel::<String>(STEER_CAP);
                    self.runner.register_steering(tid, steer_tx).await;
                    Some(steer_rx)
                } else {
                    None
                };
                // Forward each LLM token delta to the connected client as a
                // `message/delta` notification while the reply generates, stamped
                // with task_id/context_id so the client can correlate the turn.
                let (delta_tx, mut delta_rx) =
                    mpsc::channel::<crate::llm::StreamDelta>(STREAM_DELTA_CAP);
                let delta_task_id = turn_task_id.clone();
                let delta_context_id = turn_context_id.clone();
                let forward = tokio::spawn(async move {
                    while let Some(d) = delta_rx.recv().await {
                        let mut delta_params = json!({ "text": d.text, "thinking": d.thinking });
                        if let Some(t) = &delta_task_id {
                            delta_params["task_id"] = json!(t);
                        }
                        if let Some(c) = &delta_context_id {
                            delta_params["context_id"] = json!(c);
                        }
                        let note = json!({
                            "jsonrpc": "2.0",
                            "method": "message/delta",
                            "params": delta_params,
                        });
                        if notifier.send(note).await.is_err() {
                            break;
                        }
                    }
                });
                let outcome = self
                    .runner
                    .run_sync_streaming(spec, delta_tx, steer_rx)
                    .await;
                let _ = forward.await;
                if let Some(tid) = &turn_task_id {
                    self.runner.unregister_client_notifier(tid).await;
                    self.runner.unregister_steering(tid).await;
                }
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
            .handle(
                Some(user_params(Some("task-from-client"))),
                &RequestContext::none(),
            )
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
            .handle(Some(user_params(None)), &RequestContext::none())
            .await
            .expect("handle ok");
        // Runner generated its own id (prefixed "task-"), not a client id.
        let id = out.get("id").and_then(Value::as_str).unwrap_or_default();
        assert!(id.starts_with("task-"), "generated id, got {id:?}");
        assert_ne!(id, "task-from-client");
    }
}
