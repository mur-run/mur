//! `channel/consult(channel_id, nonce)` — the answerer side of channel consult
//! (design §D3, plan E1–E6).
//!
//! The dialer is neither trusted nor identified. The `Ask` event authenticates
//! itself: located by nonce, signed by its actor, bound to this agent and a
//! deadline. The asker's read scope is derived from the asker's own profile,
//! never from a request parameter. One turn runs with `read_file` only and the
//! asker's [`ConsultScope`] bound, and its answer is appended as a self-signed
//! `AskReply`.

pub mod checks;
pub mod ledger;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use mur_channel::ChannelService;
use mur_common::a2a::{Message, MessagePart};
use mur_common::channel::{AskReplyPayload, ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use serde_json::{Value, json};

use crate::llm::RequestIntent;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::consult_scope::ConsultScope;
use crate::task_runner::{TaskOutcome, TaskRunner, TaskSpec};
use checks::Refusal;
use ledger::{Claim, Ledger};

/// The A2A method name.
pub use mur_common::build::CHANNEL_CONSULT_METHOD;

/// The idempotency key of the `AskReply` to the `Ask` keyed `nonce`. Distinct
/// from the `Ask`'s own key, which the store would otherwise dedupe onto.
pub fn reply_key(nonce: &str) -> String {
    format!("{nonce}:reply")
}

pub struct ChannelConsultHandler {
    runner: Arc<TaskRunner>,
    identity: Arc<AgentIdentity>,
    /// This agent's canonical name: the only `to` it answers.
    agent: String,
    key_version: u32,
    mur_home: PathBuf,
    ledger: Arc<Ledger>,
    /// R6, resolved once. Tests clear it to exercise the other checks on
    /// every CI platform.
    platform: Option<Refusal>,
    heartbeat_interval: std::time::Duration,
}

impl ChannelConsultHandler {
    pub fn new(
        runner: Arc<TaskRunner>,
        identity: Arc<AgentIdentity>,
        agent: String,
        key_version: u32,
        mur_home: PathBuf,
        agent_home: &std::path::Path,
    ) -> Self {
        Self {
            runner,
            identity,
            agent,
            key_version,
            mur_home,
            ledger: Arc::new(Ledger::in_home(agent_home)),
            platform: checks::platform_refusal(),
            heartbeat_interval: crate::protocol::heartbeat::HEARTBEAT_INTERVAL,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_platform(mut self, platform: Option<Refusal>) -> Self {
        self.platform = platform;
        self
    }

    /// The question as the answering model sees it: who asks, and that the
    /// turn can only read what both agents may read.
    fn framed(asker: &str, question: &str) -> String {
        format!(
            "Agent `{asker}` is consulting you. Answer from what you know and from files you can \
             read with read_file. This turn can read only paths that both you and `{asker}` may \
             read; a refused read means `{asker}` may not see that path, so do not try to work \
             around it. No other tools are available.\n\nQuestion:\n{question}"
        )
    }
}

fn refuse(r: Refusal) -> HandlerError {
    match r {
        Refusal::Backend(_) | Refusal::Platform(_) => {
            HandlerError::UnsupportedCapability(r.to_string())
        }
        Refusal::BadChannel(_) => HandlerError::InvalidParams(r.to_string()),
        _ => HandlerError::CommunicationDenied(r.to_string()),
    }
}

fn str_param(p: &Value, key: &str) -> Result<String, HandlerError> {
    p.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| HandlerError::InvalidParams(format!("missing {key}")))
}

#[async_trait]
impl MethodHandler for ChannelConsultHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let channel_id = str_param(&p, "channel_id")?;
        let nonce = str_param(&p, "nonce")?;

        // §D3.0 and R6: before any event is read or any nonce recorded.
        if let Some(backend) = self.runner.non_confining_backend() {
            return Err(refuse(Refusal::Backend(backend)));
        }
        if let Some(r) = self.platform.clone() {
            return Err(refuse(r));
        }
        if !mur_channel::is_safe_channel_id(&channel_id) {
            return Err(refuse(Refusal::BadChannel(channel_id)));
        }

        // §D3.1–D3.4.
        let svc = ChannelService::open(&self.mur_home)
            .map_err(|e| HandlerError::Internal(format!("open channel store: {e}")))?;
        let events = svc
            .load_events(&channel_id)
            .map_err(|e| HandlerError::Internal(format!("load channel {channel_id}: {e}")))?;
        let now = checks::now_ms();
        let ask = checks::verify_ask(
            &self.mur_home,
            &channel_id,
            &events,
            &nonce,
            &self.agent,
            now,
        )
        .map_err(refuse)?;

        // §D3.5.
        let claim = self
            .ledger
            .claim(&ask.asker, &nonce, ask.payload.expires_at_ms, now)
            .await
            .map_err(|e| HandlerError::Internal(format!("consult ledger: {e}")))?;
        if claim == Claim::Replayed {
            return Err(refuse(Refusal::Replayed { asker: ask.asker }));
        }

        // §D3.6. The id passed §D3.2, so the join is safe. A refusal here
        // gives the claim back: no turn ran, so a later consult may answer.
        let asker_home = self.mur_home.join("agents").join(&ask.asker);
        let scope = match ConsultScope::of(ask.asker.clone(), &asker_home) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                if let Err(le) = self.ledger.release(&ask.asker, &nonce).await {
                    tracing::warn!(error = %le, "channel/consult: could not release a claim");
                }
                return Err(refuse(Refusal::Scope {
                    asker: ask.asker,
                    detail: e.to_string(),
                }));
            }
        };

        // E5: one turn, `read_file` only, the asker's scope bound.
        let task_id = uuid::Uuid::now_v7().to_string();
        let spec = TaskSpec {
            cwd: None,
            input: Message {
                role: "user".into(),
                parts: vec![MessagePart::Text {
                    text: Self::framed(&ask.asker, &ask.payload.question),
                }],
            },
            context_task_id: None,
            task_id: Some(task_id.clone()),
            active_fleet: None,
            active_team: None,
            intent: RequestIntent::Interactive,
            output_artifact_path: None,
            attended: false,
            deadline_secs: super::message_send::caller_deadline_secs(&p),
            tool_allowlist: Some(vec![crate::tools::read_file::READ_FILE.to_string()]),
            consult_scope: Some(scope),
            channel: None,
        };
        let _beat = ctx.notifier.as_ref().map(|n| {
            crate::protocol::heartbeat::spawn(n.clone(), task_id.clone(), self.heartbeat_interval)
        });
        // Nobody can answer an approval prompt for a consult.
        self.runner.mark_unattended(&task_id).await;
        self.runner.mark_delegated(&task_id).await;
        let outcome = self.runner.run_sync(spec).await;
        self.runner.unregister_client_notifier(&task_id).await;

        let task = match outcome {
            TaskOutcome::Completed(task) => task,
            TaskOutcome::Failed(task) | TaskOutcome::Cancelled(task) => {
                let why = task
                    .error
                    .as_ref()
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| format!("{:?}", task.state));
                return Err(HandlerError::Internal(format!(
                    "consult turn did not complete ({why})"
                )));
            }
        };

        // E6: the answer, signed by this agent, referencing the Ask's nonce.
        let text = super::channel_delegate::reply_text_of(&task);
        let appended = ChannelService::append_signed(
            &svc,
            &channel_id,
            &self.identity,
            self.key_version,
            ChannelActor::Agent {
                id: self.agent.clone(),
            },
            EventKind::AskReply,
            serde_json::to_value(AskReplyPayload {
                nonce: nonce.clone(),
                text: text.clone(),
            })
            .map_err(|e| HandlerError::Internal(e.to_string()))?,
            Some(reply_key(&nonce)),
        );
        let reply_seq = match appended {
            Ok(ev) => Some(ev.seq),
            Err(e) => {
                tracing::warn!(error = %e, channel_id = %channel_id, "channel/consult: AskReply append failed");
                None
            }
        };

        Ok(json!({
            "nonce": nonce,
            "asker": ask.asker,
            "text": text,
            "task_id": task.id,
            "reply_seq": reply_seq,
        }))
    }
}
