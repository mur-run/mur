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
        }
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
        }
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
            return Ok(Self::err_response(id, -32600, "jsonrpc must be '2.0'"));
        }
        match self.methods.get(&req.method) {
            Some(handler) => match handler.handle(req.params, ctx).await {
                Ok(result) => Ok(JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id,
                    result: Some(result),
                    error: None,
                }),
                Err(e) => Ok(Self::err_response(id, e.code(), &e.to_string())),
            },
            None => Ok(Self::err_response(
                id,
                -32601,
                &format!("method not found: {}", req.method),
            )),
        }
    }

    fn err_response(id: Value, code: i32, message: &str) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }
}
