//! turn/* handlers.

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::TaskRunner;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct TurnSteerHandler {
    pub runner: Arc<TaskRunner>,
}

#[async_trait]
impl MethodHandler for TurnSteerHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let task_id = p
            .get("task_id")
            .and_then(Value::as_str)
            .ok_or_else(|| HandlerError::InvalidParams("missing task_id".into()))?
            .to_string();
        let message = p
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| HandlerError::InvalidParams("missing message".into()))?;
        if message.trim().is_empty() {
            return Err(HandlerError::InvalidParams("empty steering message".into()));
        }
        self.runner
            .inject_steering(&task_id, message.to_string())
            .await?;
        Ok(json!({"task_id": task_id, "steered": true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::a2a_server::RequestContext;
    use crate::task_runner::TaskRunner;
    use serde_json::json;
    use std::sync::Arc;

    fn make_handler() -> TurnSteerHandler {
        TurnSteerHandler {
            runner: Arc::new(TaskRunner::new_stub_echo()),
        }
    }

    #[tokio::test]
    async fn turn_steer_missing_params() {
        let h = make_handler();
        let err = h.handle(None, &RequestContext::none()).await.unwrap_err();
        assert!(
            matches!(err, HandlerError::InvalidParams(_)),
            "expected InvalidParams, got {err:?}"
        );
    }

    #[tokio::test]
    async fn turn_steer_missing_task_id() {
        let h = make_handler();
        let err = h
            .handle(
                Some(json!({"message": "use ripgrep"})),
                &RequestContext::none(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn turn_steer_missing_message() {
        let h = make_handler();
        let err = h
            .handle(Some(json!({"task_id": "t1"})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn turn_steer_empty_message() {
        let h = make_handler();
        let err = h
            .handle(
                Some(json!({"task_id": "t1", "message": "   "})),
                &RequestContext::none(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn turn_steer_task_not_found() {
        let h = make_handler();
        let err = h
            .handle(
                Some(json!({"task_id": "no-such-task", "message": "pivot"})),
                &RequestContext::none(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::TaskNotFound(_)));
    }
}
