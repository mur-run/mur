//! JSON-RPC 2.0 dispatch + error code mapping (spec §8.8).

use async_trait::async_trait;
use mur_common::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Per-request context handed to each handler. Carries the **issuing
/// connection's** notification sink so streaming handlers (`message/send`) emit
/// `message/delta` / `tool/approval_needed` to *that* connection only, instead
/// of a runtime-wide broadcast that would leak one client's tokens to another.
/// `notifier` is `None` for transports that don't stream per-connection (the
/// handler then falls back to any baked-in notifier).
#[derive(Clone, Default)]
pub struct RequestContext {
    pub notifier: Option<mpsc::Sender<Value>>,
    /// Who is on the other end of this connection, and whether it has
    /// redeemed a shim ticket. `None` for transports that cannot say.
    pub conn: Option<std::sync::Arc<crate::hitl::shim_ticket::Connection>>,
    /// Whether the peer may run this agent's tools (`tools/*`). Defaults to
    /// `false`, so a transport that cannot establish the peer is this agent
    /// (TCP, or a socket peer from another lineage) fails closed. Set only by
    /// [`RequestContext::owner`] and by the unix socket transport after its
    /// lineage walk names the peer [`Caller::SelfAgent`](crate::communication_policy::Caller).
    pub runs_tools: bool,
    /// Whether the peer is the agent's owner by construction: stdio, whose
    /// peer launched the runtime. Owner methods
    /// ([`mur_common::owner_call::OWNER_METHODS`]) are served to such a peer
    /// without proof; everyone else — the agent's own lineage included —
    /// must present one. Set only by [`RequestContext::owner`].
    pub owner: bool,
    /// The running peer agent this connection's lineage names, if any. It
    /// lets a task's submitter `tasks/get` / `tasks/cancel` that task without
    /// the owner's proof (#2488). Set only by the unix socket transport.
    pub peer_agent: Option<String>,
}

impl RequestContext {
    /// A context with no per-connection notifier (single-client / non-streaming
    /// transports).
    pub fn none() -> Self {
        Self::default()
    }

    /// A context routing notifications to one connection's sink.
    pub fn with_notifier(notifier: mpsc::Sender<Value>) -> Self {
        Self {
            notifier: Some(notifier),
            conn: None,
            runs_tools: false,
            owner: false,
            peer_agent: None,
        }
    }

    /// A context for the agent's owner: stdio, whose peer is the process
    /// that launched the runtime, and in-process callers.
    pub fn owner() -> Self {
        Self {
            runs_tools: true,
            owner: true,
            ..Self::default()
        }
    }

    /// Mark whether this connection's peer may run the agent's tools.
    pub fn with_runs_tools(mut self, runs_tools: bool) -> Self {
        self.runs_tools = runs_tools;
        self
    }

    /// Refuse `method` unless the peer may run this agent's tools. A peer
    /// that passed `accepts_from` may talk to the agent; it may not borrow
    /// the agent's tools, vault and sandbox grants, or receive tool output
    /// that never taints the borrower's session (#2488).
    pub fn require_tool_access(&self, method: &str) -> Result<(), HandlerError> {
        if self.runs_tools {
            return Ok(());
        }
        Err(HandlerError::CommunicationDenied(format!(
            "{method} is served only to this agent's own processes"
        )))
    }

    /// Name the peer agent this connection's lineage resolved to.
    pub fn with_peer_agent(mut self, peer_agent: Option<String>) -> Self {
        self.peer_agent = peer_agent;
        self
    }

    /// Attach this connection's identity (unix socket transport).
    pub fn with_conn(mut self, conn: crate::hitl::shim_ticket::Connection) -> Self {
        self.conn = Some(std::sync::Arc::new(conn));
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("internal: {0}")]
    Internal(String),
    #[error("task not found: {0}")]
    TaskNotFound(String),
    #[error("task already completed: {0}")]
    TaskAlreadyCompleted(String),
    #[error("task cancelled: {0}")]
    TaskCancelled(String),
    #[error("capability not supported: {0}")]
    UnsupportedCapability(String),
    #[error("communication denied: {0}")]
    CommunicationDenied(String),
    #[error("approval expired: {0}")]
    ApprovalExpired(String),
    /// An `allow` that could not be shown to come from a human surface. Its
    /// own code so a sender can tell "you may not approve this" apart from
    /// "this approval window closed".
    #[error("approval refused: {0}")]
    ApprovalRefused(String),
    // ---- channel conversations (at-mention §7) ----
    /// `context.channel_id` names no channel directory on this host.
    #[error("unknown conversation: {0}")]
    UnknownConversation(String),
    /// The manifest was written by a newer build than this runtime.
    #[error("channel too new: {0}")]
    ChannelTooNew(String),
    /// The channel exists but is a fleet / workflow run, not a conversation.
    #[error("channel is not a conversation: {0}")]
    ChannelNotConversation(String),
    /// This agent is not a participant of the channel.
    #[error("not a participant: {0}")]
    NotAParticipant(String),
    /// A `mur channel repair` is in progress on the channel.
    #[error("conversation repairing: {0}")]
    ConversationRepairing(String),
    /// The log cannot be trusted; [`HandlerError::data`] carries the
    /// serialized damage so a client can show its `reason` (and `lines`).
    #[error("conversation damaged: {0}")]
    ConversationDamaged(mur_channel::conversation::ConversationDamage),
    /// Too many turns already wait on this conversation.
    #[error("conversation busy: {0}")]
    ConversationBusy(String),
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
            Self::ApprovalExpired(_) => -32012,
            Self::ApprovalRefused(_) => -32013,
            Self::UnknownConversation(_) => -32020,
            Self::ChannelTooNew(_) => -32021,
            Self::ChannelNotConversation(_) => -32022,
            Self::NotAParticipant(_) => -32023,
            Self::ConversationRepairing(_) => -32024,
            Self::ConversationDamaged(_) => -32025,
            Self::ConversationBusy(_) => -32026,
        }
    }

    /// Structured `JsonRpcError.data` for errors that carry more than text.
    pub fn data(&self) -> Option<Value> {
        match self {
            Self::ConversationDamaged(d) => serde_json::to_value(d).ok(),
            _ => None,
        }
    }
}

#[async_trait]
pub trait MethodHandler: Send + Sync {
    async fn handle(
        &self,
        params: Option<Value>,
        ctx: &RequestContext,
    ) -> Result<Value, HandlerError>;
}

pub struct Dispatcher {
    methods: HashMap<String, Box<dyn MethodHandler>>,
    owner_gate: Option<OwnerGate>,
    submitters: super::task_submitters::TaskSubmitters,
}

/// What the dispatcher needs to check an owner-method proof: the name the
/// proof is bound to and the home token it is keyed with.
struct OwnerGate {
    agent: String,
    authority: crate::hitl::authority::ApprovalAuthority,
}

impl Default for Dispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Dispatcher {
    pub fn new() -> Self {
        Self {
            methods: HashMap::new(),
            owner_gate: None,
            submitters: Default::default(),
        }
    }

    /// Check owner-method proofs as agent `agent`, keyed by `authority`'s
    /// token. Without this, an owner method is served to stdio only.
    pub fn with_owner_gate(
        mut self,
        agent: &str,
        authority: crate::hitl::authority::ApprovalAuthority,
    ) -> Self {
        self.owner_gate = Some(OwnerGate {
            agent: agent.to_string(),
            authority,
        });
        self
    }

    /// Every registered method name, for coverage tests.
    pub fn method_names(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }

    /// Admit an owner method from stdio or with a valid proof; refuse it
    /// otherwise (#2488). The proof is removed from `params` either way, so
    /// no handler sees it and a non-owner method never carries one inward.
    fn gate_owner(
        &self,
        method: &str,
        params: &mut Option<Value>,
        ctx: &RequestContext,
    ) -> Result<(), HandlerError> {
        let presented = params
            .as_mut()
            .and_then(Value::as_object_mut)
            .and_then(|o| o.remove(mur_common::owner_call::PARAM));
        if !mur_common::owner_call::is_owner_method(method) || ctx.owner {
            return Ok(());
        }
        if self.is_submitter_call(method, params.as_ref(), ctx) {
            return Ok(());
        }
        let deny = |why: &str| {
            Err(HandlerError::CommunicationDenied(format!(
                "{method} is served only to this agent's owner: {why}"
            )))
        };
        let Some(gate) = &self.owner_gate else {
            return deny("this transport cannot verify the owner");
        };
        if !gate.authority.has_token() {
            return deny(
                "the agent could not load the approval token when it started; \
                 check the agent log, then restart the agent",
            );
        }
        match presented.as_ref().and_then(Value::as_str) {
            Some(p) if gate.authority.verify_owner(&gate.agent, method, p) => Ok(()),
            Some(_) => deny("the owner proof does not match this MUR home"),
            None => deny(
                "no owner proof; call it from the Hub or the `mur` CLI \
                 running as the same user and MUR_HOME",
            ),
        }
    }

    /// Claim the caller-named `task_id` of a turn-starting call for the
    /// connection's peer agent (or for no agent), first claim wins.
    fn claim_task(&self, method: &str, params: Option<&Value>, ctx: &RequestContext) {
        use super::task_submitters::SUBMIT_METHODS;
        if !SUBMIT_METHODS.contains(&method) {
            return;
        }
        if let Some(id) = params
            .and_then(|p| p.get("task_id"))
            .and_then(Value::as_str)
        {
            self.submitters.record(id, ctx.peer_agent.as_deref());
        }
    }

    /// True when a peer agent asks about, or stops, a task it submitted.
    fn is_submitter_call(
        &self,
        method: &str,
        params: Option<&Value>,
        ctx: &RequestContext,
    ) -> bool {
        use super::task_submitters::SUBMITTER_METHODS;
        let (Some(agent), true) = (
            ctx.peer_agent.as_deref(),
            SUBMITTER_METHODS.contains(&method),
        ) else {
            return false;
        };
        params
            .and_then(|p| p.get("id"))
            .and_then(Value::as_str)
            .is_some_and(|id| self.submitters.is_submitter(id, agent))
    }

    pub fn register(&mut self, name: &str, handler: Box<dyn MethodHandler>) {
        self.methods.insert(name.to_string(), handler);
    }

    pub async fn dispatch(
        &self,
        req: JsonRpcRequest,
        ctx: &RequestContext,
    ) -> Result<JsonRpcResponse, HandlerError> {
        let id = req.id.clone().unwrap_or(json!(null));
        if req.jsonrpc != "2.0" {
            return Ok(Self::err_response(
                id,
                -32600,
                "jsonrpc must be '2.0'",
                None,
            ));
        }
        let mut params = req.params;
        if self.methods.contains_key(&req.method)
            && let Err(e) = self.gate_owner(&req.method, &mut params, ctx)
        {
            tracing::warn!(method = %req.method, "refused an owner method from a non-owner peer");
            return Ok(Self::err_response(id, e.code(), &e.to_string(), e.data()));
        }
        if self.methods.contains_key(&req.method) {
            self.claim_task(&req.method, params.as_ref(), ctx);
        }
        match self.methods.get(&req.method) {
            Some(handler) => match handler.handle(params, ctx).await {
                Ok(result) => Ok(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id,
                    result: Some(result),
                    error: None,
                }),
                Err(e) => Ok(Self::err_response(id, e.code(), &e.to_string(), e.data())),
            },
            None => Ok(Self::err_response(
                id,
                -32601,
                &format!("method not found: {}", req.method),
                None,
            )),
        }
    }

    fn err_response(id: Value, code: i32, message: &str, data: Option<Value>) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data,
            }),
        }
    }
}

#[cfg(test)]
#[path = "a2a_server_owner_tests.rs"]
mod owner_tests;
