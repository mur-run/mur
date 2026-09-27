//! `shim/hello` — a CLI-spawn turn's MCP shim redeeming its one-time ticket.
//! The rules live in `hitl::shim_ticket`; this is only the wire.

use crate::hitl::shim_ticket::ShimTrust;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct ShimHelloHandler {
    pub trust: ShimTrust,
}

#[async_trait]
impl MethodHandler for ShimHelloHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let field = |k: &str| {
            p[k].as_str()
                .ok_or_else(|| HandlerError::InvalidParams(format!("missing {k}")))
        };
        let (task_id, ticket) = (field("task_id")?, field("ticket")?);
        let conn = ctx.conn.as_deref().ok_or_else(|| {
            HandlerError::ApprovalRefused("this transport cannot carry shim trust".into())
        })?;
        self.trust.redeem(conn, task_id, ticket).map_err(|r| {
            tracing::warn!(task_id = %task_id, refusal = ?r, "refused a shim hello");
            HandlerError::ApprovalRefused(r.message().to_string())
        })?;
        Ok(json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_hello_without_a_connection_identity_is_refused() {
        let trust = ShimTrust::default();
        let t = trust.issue("t1");
        let err = ShimHelloHandler {
            trust: trust.clone(),
        }
        .handle(
            Some(json!({"task_id": "t1", "ticket": t.ticket})),
            &RequestContext::none(),
        )
        .await
        .expect_err("no conn");
        assert!(matches!(err, HandlerError::ApprovalRefused(_)));
    }

    #[tokio::test]
    async fn a_hello_from_a_non_descendant_is_refused() {
        // This test process is not a child of pid 1's ... anything we bind;
        // binding our own parent as "the CLI" makes us a descendant, binding
        // our own pid does not (strict).
        let trust = ShimTrust::default();
        let t = trust.issue("t1");
        t.bind_cli_pid(std::process::id());
        let ctx = RequestContext::default().with_conn(crate::hitl::shim_ticket::Connection::new(
            Some(std::process::id()),
        ));
        let err = ShimHelloHandler { trust }
            .handle(Some(json!({"task_id": "t1", "ticket": t.ticket})), &ctx)
            .await
            .expect_err("self is not a strict descendant of self");
        assert!(matches!(err, HandlerError::ApprovalRefused(_)));
    }
}
