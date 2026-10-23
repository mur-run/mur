//! tasks/* handlers.

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::TaskRunner;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct TasksGetHandler {
    runner: Arc<TaskRunner>,
}

impl TasksGetHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { runner: r }
    }
}

#[async_trait]
impl MethodHandler for TasksGetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let id = params
            .and_then(|p| p.get("id").and_then(|v| v.as_str().map(String::from)))
            .ok_or_else(|| HandlerError::InvalidParams("missing id".into()))?;
        match self.runner.get_state(&id) {
            Some(state) => {
                let mut out = json!({"id": id, "state": state});
                // Present once the turn has ended: what it really spent, so a
                // caller whose reply was lost can still account it (#2083).
                if let Some(usage) = self.runner.get_usage(&id) {
                    out["usage"] = usage;
                }
                Ok(out)
            }
            None => Err(HandlerError::TaskNotFound(id)),
        }
    }
}

pub struct TasksCancelHandler {
    runner: Arc<TaskRunner>,
}

impl TasksCancelHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { runner: r }
    }
}

#[async_trait]
impl MethodHandler for TasksCancelHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let id = params
            .and_then(|p| p.get("id").and_then(|v| v.as_str().map(String::from)))
            .ok_or_else(|| HandlerError::InvalidParams("missing id".into()))?;
        self.runner
            .cancel(&id)
            .await
            .map_err(|_| HandlerError::TaskNotFound(id.clone()))?;
        Ok(json!({"id": id, "state": "cancelled"}))
    }
}

pub struct TasksListHandler {
    _runner: Arc<TaskRunner>,
}

impl TasksListHandler {
    pub fn new(r: Arc<TaskRunner>) -> Self {
        Self { _runner: r }
    }
}

#[async_trait]
impl MethodHandler for TasksListHandler {
    async fn handle(
        &self,
        _params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        // P0a: return empty list; full history comes when we persist TaskRunner state.
        Ok(json!([]))
    }
}
