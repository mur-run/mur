//! A2A methods `hitl/list` and `hitl/forget` — gate B's remembered decisions
//! (#1806).
//!
//! Served by the running runtime because a revocation is a signed event and
//! the runtime is what holds the agent key: a CLI write path would widen the
//! key's exposure and duplicate the signing path that #1770's chain covers.
//! Responses carry hashes, verdicts, times and the stored redacted label —
//! never tool input.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::hitl::store::ChannelDecisionStore;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};

/// `hitl/forget` params: exactly one of these.
const PARAM_HASH_PREFIX: &str = "hash_prefix";
const PARAM_ALL: &str = "all";

pub struct HitlListHandler {
    store: Arc<ChannelDecisionStore>,
}

pub struct HitlForgetHandler {
    store: Arc<ChannelDecisionStore>,
}

impl HitlListHandler {
    pub fn new(store: Arc<ChannelDecisionStore>) -> Self {
        Self { store }
    }
}

impl HitlForgetHandler {
    pub fn new(store: Arc<ChannelDecisionStore>) -> Self {
        Self { store }
    }
}

fn internal(e: anyhow::Error) -> HandlerError {
    HandlerError::Internal(format!("{e:#}"))
}

#[async_trait]
impl MethodHandler for HitlListHandler {
    async fn handle(&self, _p: Option<Value>, _c: &RequestContext) -> Result<Value, HandlerError> {
        let store = self.store.clone();
        let live = tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|e| HandlerError::Internal(e.to_string()))?
            .map_err(internal)?;
        Ok(json!({ "decisions": live }))
    }
}

/// `Some(prefix)` = one, `None` = all. Neither or both is a usage error.
fn forget_target(params: &Option<Value>) -> Result<Option<String>, HandlerError> {
    let p = params.as_ref();
    let prefix = p
        .and_then(|p| p.get(PARAM_HASH_PREFIX))
        .and_then(Value::as_str);
    let all = p.and_then(|p| p.get(PARAM_ALL)).and_then(Value::as_bool) == Some(true);
    match (prefix, all) {
        (Some(h), false) => Ok(Some(h.to_string())),
        (None, true) => Ok(None),
        _ => Err(HandlerError::InvalidParams(format!(
            "pass exactly one of '{PARAM_HASH_PREFIX}' or '{PARAM_ALL}: true'"
        ))),
    }
}

#[async_trait]
impl MethodHandler for HitlForgetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _c: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let target = forget_target(&params)?;
        let store = self.store.clone();
        let revoked = tokio::task::spawn_blocking(move || store.forget(target.as_deref()))
            .await
            .map_err(|e| HandlerError::Internal(e.to_string()))?
            // Unknown / ambiguous prefix: the caller's input, not a crash.
            .map_err(|e| HandlerError::InvalidParams(format!("{e:#}")))?;
        tracing::info!(count = revoked.len(), "hitl/forget: decisions revoked");
        Ok(json!({ "revoked": revoked }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forget_needs_exactly_one_target() {
        assert_eq!(
            forget_target(&Some(json!({"hash_prefix": "ab"}))).unwrap(),
            Some("ab".into())
        );
        assert_eq!(forget_target(&Some(json!({"all": true}))).unwrap(), None);
        assert!(forget_target(&None).is_err());
        assert!(forget_target(&Some(json!({"all": false}))).is_err());
        assert!(forget_target(&Some(json!({"hash_prefix": "ab", "all": true}))).is_err());
    }
}
