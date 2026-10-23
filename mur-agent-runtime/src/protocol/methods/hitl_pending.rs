//! A2A method `hitl/pending` (#2085): the approvals a headless turn is
//! waiting on right now, so `mur agent hitl approve` can answer one from
//! another terminal.
//!
//! Read-only. Answering goes through the unchanged `tool/hitl_respond`, whose
//! approval authority (enforcing sandbox + the home's approval token) is what
//! makes an allow a human's — listing a `hitl_id` grants nothing. Entries carry
//! the redacted label only, never tool input.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::hitl::parked::Parking;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};

pub struct HitlPendingHandler {
    parking: Parking,
}

impl HitlPendingHandler {
    pub fn new(parking: Parking) -> Self {
        Self { parking }
    }
}

#[async_trait]
impl MethodHandler for HitlPendingHandler {
    async fn handle(&self, _p: Option<Value>, _c: &RequestContext) -> Result<Value, HandlerError> {
        Ok(json!({ "pending": self.parking.list() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hitl::parked::ParkedCall;

    #[tokio::test]
    async fn lists_parked_calls_without_tool_input() {
        let parking = Parking::default();
        let now = chrono::Utc::now();
        parking.park(ParkedCall {
            hitl_id: "h-1".into(),
            task_id: "t-1".into(),
            tool_name: "bash".into(),
            label: "bash: rm …".into(),
            parked_at: now,
            expires_at: now,
        });
        let v = HitlPendingHandler::new(parking)
            .handle(None, &RequestContext::none())
            .await
            .unwrap();
        let first = &v["pending"][0];
        assert_eq!(first["hitl_id"], "h-1");
        assert_eq!(first["task_id"], "t-1");
        assert!(first.get("tool_input").is_none(), "{v}");
    }
}
