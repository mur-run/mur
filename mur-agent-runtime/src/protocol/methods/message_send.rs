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
    /// `turn/heartbeat` cadence; the constant in production, shortened by tests.
    heartbeat_interval: std::time::Duration,
}

impl MessageSendHandler {
    pub fn new(runner: Arc<TaskRunner>) -> Self {
        Self {
            runner,
            progress: None,
            notifier: None,
            heartbeat_interval: crate::protocol::heartbeat::HEARTBEAT_INTERVAL,
        }
    }

    pub fn with_progress(runner: Arc<TaskRunner>, progress: mpsc::Sender<Event>) -> Self {
        Self {
            runner,
            progress: Some(progress),
            notifier: None,
            heartbeat_interval: crate::protocol::heartbeat::HEARTBEAT_INTERVAL,
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
    /// Shorten the beat so a test can see one inside its budget.
    #[cfg(test)]
    pub(crate) fn with_heartbeat_interval(mut self, every: std::time::Duration) -> Self {
        self.heartbeat_interval = every;
        self
    }
}

/// `params.limits.deadline_secs`, when the caller sent one. Negative or
/// non-integer values read as absent — the resolver then applies the scopes,
/// which is the safe direction (a bound, not none).
pub(crate) fn caller_deadline_secs(p: &Value) -> Option<u64> {
    p.get("limits")
        .and_then(|l| l.get("deadline_secs"))
        .and_then(|v| v.as_u64())
}

/// `params.needs`: the tools the brief declares it requires (a fleet's
/// `needs:`). Absent → empty → no preflight.
pub(crate) fn declared_needs(p: &Value) -> Vec<String> {
    p.get("needs")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A task that never started (spec §3.8): the input, a terminal Failed
/// state, and the reason as a non-recoverable error — no model call, no
/// tool, no settlement to draw.
pub(crate) fn failed_task_before_start(
    task_id: Option<String>,
    input: Message,
    code: &str,
    message: &str,
) -> mur_common::a2a::Task {
    let now = chrono::Utc::now().to_rfc3339();
    mur_common::a2a::Task {
        id: task_id.unwrap_or_else(|| format!("task-{}", uuid::Uuid::now_v7())),
        state: mur_common::a2a::TaskState::Failed,
        messages: vec![input],
        created_at: now.clone(),
        completed_at: Some(now),
        error: Some(mur_common::a2a::TaskError {
            code: code.to_string(),
            message: message.to_string(),
            recoverable: false,
            details: None,
        }),
        usage: None,
        artifacts: None,
    }
}

/// The dispatch preflight (spec §3.8): the first declared need this runtime
/// cannot offer, phrased as the grant command.
pub(crate) fn preflight_missing(agent: &str, missing: &[String]) -> Option<String> {
    missing.first().map(|tool| {
        format!("cannot start: {agent} has no {tool} — mur agent perm tool-allow {agent} {tool}")
    })
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
        // Whether a human is reading this connection and can answer an approval
        // prompt. Absent means yes — every client that predates this field is
        // an interactive one, and defaulting the other way would make them all
        // start refusing gated tools. `mur agent send` passes false.
        let can_approve = p
            .get("can_approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
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
        // The caller's working directory, sent on every turn by murmur so the
        // runtime — not the model's memory — owns where relative paths go.
        let cwd = p
            .get("context")
            .and_then(|c| c.get("cwd"))
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let spec = TaskSpec {
            cwd,
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
            attended: can_approve,
            deadline_secs: caller_deadline_secs(&p),
        };

        // Fail at dispatch, not after the budget (spec §3.8): a declared need
        // this runtime cannot offer ends the task before any model call.
        let missing = self.runner.missing_tools(&declared_needs(&p));
        if let Some(msg) = preflight_missing(self.runner.agent_name(), &missing) {
            let task =
                failed_task_before_start(spec.task_id.clone(), spec.input, "cannot_start", &msg);
            return serde_json::to_value(&task).map_err(|e| HandlerError::Internal(e.to_string()));
        }

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
                        .register_client_notifier(tid, notifier.clone(), can_approve)
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
                // Proof of life for the dialing side while this turn runs —
                // including through model inference, when no delta flows.
                let _beat = turn_task_id.as_ref().map(|tid| {
                    crate::protocol::heartbeat::spawn(
                        notifier.clone(),
                        tid.clone(),
                        self.heartbeat_interval,
                    )
                });
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

#[cfg(test)]
mod limits_params {
    /// The wire shape both handlers read: `limits.deadline_secs` is the
    /// caller's remaining clock; absent means "resolve from the scopes".
    #[test]
    fn deadline_secs_is_read_from_limits_and_absent_is_none() {
        let p = serde_json::json!({"limits": {"deadline_secs": 720}});
        assert_eq!(super::caller_deadline_secs(&p), Some(720));
        assert_eq!(super::caller_deadline_secs(&serde_json::json!({})), None);
        assert_eq!(
            super::caller_deadline_secs(&serde_json::json!({"limits": {"deadline_secs": -1}})),
            None
        );
    }
}
