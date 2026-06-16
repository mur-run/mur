//! `channel/delegate` (v3d-2): a delegated specialist runs the sub-goal and
//! appends its OWN reply, signed by its own identity, attributed to Agent{self}.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use mur_channel::ChannelService;
use mur_common::a2a::{Message, MessagePart};
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use serde_json::Value;

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};

/// Append the specialist's reply to `channel_id` as `Agent{self}`, signed by the
/// specialist's identity (v3d-2 peer-writes-own).
#[allow(clippy::too_many_arguments)]
pub fn append_self_reply(
    mur_home: &Path,
    channel_id: &str,
    agent: &str,
    identity: &AgentIdentity,
    key_version: u32,
    reply_text: &str,
    task_id: &str,
    idem: Option<String>,
) -> anyhow::Result<()> {
    let svc = ChannelService::open(mur_home)?;
    svc.append_signed(
        channel_id,
        identity,
        key_version,
        ChannelActor::Agent {
            id: agent.to_string(),
        },
        EventKind::Message,
        serde_json::json!({ "text": reply_text, "task_id": task_id }),
        idem,
    )?;
    Ok(())
}

/// Extract the agent's reply text from a finished [`Task`](mur_common::a2a::Task):
/// the last message's first text part. Returns an empty string if the task has
/// no messages or the final message carries no text part.
fn reply_text_of(task: &mur_common::a2a::Task) -> String {
    task.messages
        .last()
        .and_then(|m| {
            m.parts.iter().find_map(|p| match p {
                MessagePart::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default()
}

/// `channel/delegate` handler. Runs the agent turn exactly like `message/send`
/// (non-streaming `run_sync` path), then ALSO appends the reply to the channel
/// as a signed `Agent{self}` event via [`append_self_reply`] before returning
/// the `Task` JSON. The self-append is best-effort: a failure is logged but does
/// not fail the RPC (the turn already succeeded).
pub struct ChannelDelegateHandler {
    runner: Arc<TaskRunner>,
    identity: Arc<AgentIdentity>,
    agent: String,
    key_version: u32,
    mur_home: PathBuf,
}

impl ChannelDelegateHandler {
    pub fn new(
        runner: Arc<TaskRunner>,
        identity: Arc<AgentIdentity>,
        agent: String,
        key_version: u32,
        mur_home: PathBuf,
    ) -> Self {
        Self {
            runner,
            identity,
            agent,
            key_version,
            mur_home,
        }
    }
}

#[async_trait]
impl MethodHandler for ChannelDelegateHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        // NEW vs message/send: a channel_id is required so we know where to
        // append the self-reply.
        let channel_id = p
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| HandlerError::InvalidParams("missing channel_id".into()))?;
        let idem = p
            .get("idempotency_key")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Param parsing mirrors message/send: message → Message, optional
        // task_id and context.task_id → TaskSpec.
        let message: Message = serde_json::from_value(p["message"].clone())
            .map_err(|e| HandlerError::InvalidParams(format!("message: {e}")))?;
        let context_task_id = p
            .get("context")
            .and_then(|c| c.get("task_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let task_id = p
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let spec = TaskSpec {
            input: message,
            context_task_id,
            task_id,
        };

        // Run the turn (non-streaming path; v3d-2 does not need per-delta
        // forwarding for delegated specialist replies).
        let outcome = self.runner.run_sync(spec).await;
        // Only a Completed turn carries a genuine agent reply (`messages` =
        // [input, reply]); Failed/Cancelled tasks carry only the user input, so
        // appending their "last message" would sign the user's own text as the
        // specialist's reply. Skip the channel write for those — the RPC still
        // returns the Task so the caller sees the failure/cancellation.
        let completed = matches!(outcome, TaskOutcome::Completed(_));
        let task = match outcome {
            TaskOutcome::Completed(task)
            | TaskOutcome::Failed(task)
            | TaskOutcome::Cancelled(task) => task,
        };

        // Append the specialist's own signed reply to the channel. Best-effort:
        // a completed turn must still be returned even if the channel write
        // fails (e.g. channel missing / store error).
        if completed {
            let reply = reply_text_of(&task);
            if let Err(e) = append_self_reply(
                &self.mur_home,
                &channel_id,
                &self.agent,
                &self.identity,
                self.key_version,
                &reply,
                &task.id,
                idem,
            ) {
                tracing::warn!(
                    error = %e,
                    channel_id = %channel_id,
                    agent = %self.agent,
                    task_id = %task.id,
                    "channel/delegate: failed to append self-reply to channel"
                );
            }
        }

        serde_json::to_value(&task).map_err(|e| HandlerError::Internal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_self_reply_is_signed_by_the_specialist() {
        let tmp = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        append_self_reply(tmp.path(), &ch.id, "qa", &id, 0, "the answer", "t-1", None).unwrap();
        let evs = svc.load_events(&ch.id).unwrap();
        let reply = evs
            .iter()
            .rev()
            .find(|e| {
                e.kind == EventKind::Message
                    && matches!(&e.actor, ChannelActor::Agent { id } if id == "qa")
            })
            .unwrap();
        assert_eq!(reply.payload["text"], "the answer");
        assert!(mur_channel::sign::verify_one(
            &ch.id,
            reply,
            &id.verifying_key_bytes(),
            true
        ));
    }
}
