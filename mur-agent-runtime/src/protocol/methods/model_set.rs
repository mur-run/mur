//! A2A method: `model/set` — hot-switch the running agent to another registry
//! model. The live process only: the runtime is sealed off from its own
//! `profile.yaml` (the launch chain denies that write — the profile is the
//! operator's file, see `sandbox::launch_chain`), so persisting the choice is
//! the caller's job. murmur `/model` writes the profile first and then dials
//! this, the same dual-write `/secret` uses; `mur agent dial` callers get the
//! live switch and `"persisted": false` telling them the disk is theirs.
//! Before this the handler persisted before swapping, and on a sandboxed
//! runtime that write failed every time — no agent ever hot-switched.
//!
//! Registered when boot produced a [`ModelSwitchHandle`]: single-model agents
//! swap the client, chain/routing agents swap the primary and keep the chain.
//! Echo and misconfigured agents surface method-not-found, which murmur
//! degrades to the profile write plus a restart hint.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::llm::switchable::ModelSwitchHandle;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};

pub struct ModelSetHandler {
    switch: Arc<ModelSwitchHandle>,
}

impl ModelSetHandler {
    pub fn new(switch: Arc<ModelSwitchHandle>) -> Self {
        Self { switch }
    }
}

#[async_trait]
impl MethodHandler for ModelSetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let params = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let model_ref = params
            .get("model_ref")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing 'model_ref' field".into()))?;

        // Build first, swap second: a ref the builder rejects leaves the old
        // client running and nothing half-switched.
        let next = (self.switch.build_client)(model_ref)
            .map_err(|e| HandlerError::InvalidParams(format!("model_ref {model_ref:?}: {e:#}")))?;
        self.switch.switchable.swap(next);
        tracing::info!(model_ref, "model/set: live client switched");
        Ok(json!({ "model_ref": model_ref, "effective": "next-turn", "persisted": false }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::switchable::SwitchableLlmClient;
    use crate::llm::{LlmClient, LlmError, LlmRequest, LlmResponse, RequestIntent, StopReason};

    struct FixedClient {
        name: &'static str,
    }

    #[async_trait]
    impl LlmClient for FixedClient {
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: self.name.to_string(),
                input_tokens: 0,
                output_tokens: 0,
                model: self.name.to_string(),
                tool_calls: vec![],
                stop_reason: StopReason::EndTurn,
            })
        }

        fn model_name(&self) -> &str {
            self.name
        }
    }

    fn handle_with(
        factory: crate::llm::fallback::ClientFactory,
    ) -> (ModelSetHandler, Arc<ModelSwitchHandle>) {
        let handle = Arc::new(ModelSwitchHandle {
            switchable: SwitchableLlmClient::new(Arc::new(FixedClient { name: "boot" })),
            build_client: factory,
        });
        (ModelSetHandler::new(handle.clone()), handle)
    }

    fn req() -> LlmRequest {
        LlmRequest {
            messages: vec![],
            temperature: None,
            max_tokens: None,
            tools: vec![],
            intent: RequestIntent::Interactive,
            pin_model_ref: None,
            task_id: None,
            effort: None,
        }
    }

    async fn answers(handle: &ModelSwitchHandle) -> String {
        handle.switchable.generate(req()).await.unwrap().text
    }

    /// The live client switches and the reply says the disk is still the
    /// caller's to write — the runtime cannot reach its own profile.
    #[tokio::test]
    async fn switches_the_live_client_and_says_it_did_not_persist() {
        let (h, handle) = handle_with(Box::new(|_ref| {
            Ok(Arc::new(FixedClient { name: "switched" }) as _)
        }));
        assert_eq!(answers(&handle).await, "boot");

        let out = h
            .handle(
                Some(json!({"model_ref": "new_ref"})),
                &RequestContext::none(),
            )
            .await
            .unwrap();
        assert_eq!(out["model_ref"], "new_ref");
        assert_eq!(out["effective"], "next-turn");
        assert_eq!(out["persisted"], false);
        assert_eq!(answers(&handle).await, "switched");
    }

    #[tokio::test]
    async fn builder_failure_aborts_without_swapping() {
        let (h, handle) = handle_with(Box::new(|r| {
            anyhow::bail!("model_ref {r:?} not in registry")
        }));

        let err = h
            .handle(Some(json!({"model_ref": "ghost"})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
        assert_eq!(
            answers(&handle).await,
            "boot",
            "old client must keep running"
        );
    }

    #[tokio::test]
    async fn missing_model_ref_param_is_invalid() {
        let (h, _) = handle_with(Box::new(|_r| Ok(Arc::new(FixedClient { name: "x" }) as _)));
        let err = h
            .handle(Some(json!({})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }
}
