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

use crate::hitl::store::{ChannelDecisionStore, Listing};
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};

/// `hitl/forget` params: exactly one of these.
const PARAM_HASH_PREFIX: &str = "hash_prefix";
const PARAM_ALL: &str = "all";
/// A target of its own, never a modifier of `all`: a runtime that predates it
/// rejects the call instead of silently forgetting the allows too.
const PARAM_DENIES_ONLY: &str = "denies_only";

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

/// An untrusted log is a normal state, not a runtime fault: no decisions, plus
/// the reason and channel id so the caller can show the way out (#2033).
fn listing_json(listing: Listing) -> Value {
    match listing {
        Listing::Live(decisions) => json!({ "decisions": decisions }),
        Listing::Unavailable { reason, channel_id } => json!({
            "decisions": [],
            "unavailable": { "reason": reason, "channel_id": channel_id },
        }),
    }
}

#[async_trait]
impl MethodHandler for HitlListHandler {
    async fn handle(&self, _p: Option<Value>, _c: &RequestContext) -> Result<Value, HandlerError> {
        let store = self.store.clone();
        let listing = tokio::task::spawn_blocking(move || store.list_report())
            .await
            .map_err(|e| HandlerError::Internal(e.to_string()))?
            .map_err(internal)?;
        Ok(listing_json(listing))
    }
}

/// What `hitl/forget` was asked to revoke.
#[derive(Debug, PartialEq, Eq)]
enum ForgetTarget {
    Prefix(String),
    All,
    DeniesOnly,
}

/// Exactly one target; none or several is a usage error.
fn forget_target(params: &Option<Value>) -> Result<ForgetTarget, HandlerError> {
    let p = params.as_ref();
    let prefix = p
        .and_then(|p| p.get(PARAM_HASH_PREFIX))
        .and_then(Value::as_str);
    let flag = |k: &str| p.and_then(|p| p.get(k)).and_then(Value::as_bool) == Some(true);
    match (prefix, flag(PARAM_ALL), flag(PARAM_DENIES_ONLY)) {
        (Some(h), false, false) => Ok(ForgetTarget::Prefix(h.to_string())),
        (None, true, false) => Ok(ForgetTarget::All),
        (None, false, true) => Ok(ForgetTarget::DeniesOnly),
        _ => Err(HandlerError::InvalidParams(format!(
            "pass exactly one of '{PARAM_HASH_PREFIX}', '{PARAM_ALL}: true' or '{PARAM_DENIES_ONLY}: true'"
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
        let revoked = tokio::task::spawn_blocking(move || match target {
            ForgetTarget::Prefix(p) => store.forget(Some(&p)),
            ForgetTarget::All => store.forget(None),
            ForgetTarget::DeniesOnly => store.forget_denies(),
        })
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
    fn unavailable_listing_is_a_result_with_reason_and_channel() {
        let v = listing_json(Listing::Unavailable {
            reason: "log has records but no signed head".into(),
            channel_id: "c1".into(),
        });
        assert_eq!(v["decisions"], json!([]));
        assert_eq!(v["unavailable"]["channel_id"], "c1");
        assert_eq!(
            v["unavailable"]["reason"],
            "log has records but no signed head"
        );
        assert!(
            listing_json(Listing::Live(vec![]))
                .get("unavailable")
                .is_none()
        );
    }

    #[test]
    fn forget_needs_exactly_one_target() {
        assert_eq!(
            forget_target(&Some(json!({"hash_prefix": "ab"}))).unwrap(),
            ForgetTarget::Prefix("ab".into())
        );
        assert_eq!(
            forget_target(&Some(json!({"all": true}))).unwrap(),
            ForgetTarget::All
        );
        assert_eq!(
            forget_target(&Some(json!({"denies_only": true}))).unwrap(),
            ForgetTarget::DeniesOnly
        );
        assert!(forget_target(&None).is_err());
        assert!(forget_target(&Some(json!({"all": false}))).is_err());
        assert!(forget_target(&Some(json!({"hash_prefix": "ab", "all": true}))).is_err());
        // Combined with `all`, denies-only must not quietly mean "all".
        assert!(forget_target(&Some(json!({"all": true, "denies_only": true}))).is_err());
        assert!(forget_target(&Some(json!({"hash_prefix": "ab", "denies_only": true}))).is_err());
    }
}
