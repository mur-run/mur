//! A2A method: `model/set` — hot-switch the running agent to another registry
//! model and persist the choice to `profile.yaml`.
//!
//! Registered only when boot produced a [`ModelSwitchHandle`] (single-model
//! agents). Chain/routing and echo agents surface method-not-found, which the
//! murmur TUI degrades to a profile write + restart hint.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::llm::switchable::ModelSwitchHandle;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};

pub struct ModelSetHandler {
    switch: Arc<ModelSwitchHandle>,
    profile_path: PathBuf,
}

impl ModelSetHandler {
    pub fn new(switch: Arc<ModelSwitchHandle>, profile_path: PathBuf) -> Self {
        Self {
            switch,
            profile_path,
        }
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

        // Strict order: build → persist → swap. Any failure aborts with the
        // old client AND the old profile intact — a switch can never leave
        // "disk says new, process runs old" (or the reverse) behind.
        let next = (self.switch.build_client)(model_ref)
            .map_err(|e| HandlerError::InvalidParams(format!("model_ref {model_ref:?}: {e:#}")))?;
        persist_model_ref(&self.profile_path, model_ref)
            .map_err(|e| HandlerError::Internal(format!("persist model_ref: {e:#}")))?;
        self.switch.switchable.swap(next);
        tracing::info!(model_ref, "model/set: live client switched");
        Ok(json!({ "model_ref": model_ref, "effective": "next-turn" }))
    }
}

/// Rewrite `model_ref` in `profile.yaml` — typed round-trip + temp/rename,
/// the same idiom as the rekey cleanup. The legacy `model:` block is a live
/// read path and stays untouched; `model_ref` wins at resolution.
fn persist_model_ref(path: &Path, model_ref: &str) -> anyhow::Result<()> {
    let yaml = std::fs::read_to_string(path)?;
    let mut p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml)?;
    p.model_ref = Some(model_ref.to_string());
    p.updated_at = chrono::Utc::now().to_rfc3339();
    let out = serde_yaml_ng::to_string(&p)?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, out.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::switchable::SwitchableLlmClient;
    use crate::llm::{LlmClient, LlmError, LlmRequest, LlmResponse, StopReason};

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

    const MINIMAL_PROFILE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../mur-common/tests/fixtures/profile_p0a_minimal.yaml"
    ));

    fn seed_profile(dir: &Path) -> PathBuf {
        let path = dir.join("profile.yaml");
        let mut p: mur_common::agent::AgentProfile =
            serde_yaml_ng::from_str(MINIMAL_PROFILE).unwrap();
        p.model_ref = Some("old_ref".into());
        std::fs::write(&path, serde_yaml_ng::to_string(&p).unwrap()).unwrap();
        path
    }

    fn handle_with(
        factory: crate::llm::fallback::ClientFactory,
        profile_path: PathBuf,
    ) -> ModelSetHandler {
        ModelSetHandler::new(
            Arc::new(ModelSwitchHandle {
                switchable: SwitchableLlmClient::new(Arc::new(FixedClient { name: "boot" })),
                build_client: factory,
            }),
            profile_path,
        )
    }

    #[tokio::test]
    async fn switches_persists_and_replies() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_path = seed_profile(tmp.path());
        let h = handle_with(
            Box::new(|_ref| Ok(Arc::new(FixedClient { name: "switched" }) as _)),
            profile_path.clone(),
        );

        let out = h
            .handle(
                Some(json!({"model_ref": "new_ref"})),
                &RequestContext::none(),
            )
            .await
            .unwrap();
        assert_eq!(out["model_ref"], "new_ref");
        assert_eq!(out["effective"], "next-turn");

        let p: mur_common::agent::AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path).unwrap()).unwrap();
        assert_eq!(p.model_ref.as_deref(), Some("new_ref"));
    }

    #[tokio::test]
    async fn builder_failure_aborts_without_persisting_or_swapping() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profile_path = seed_profile(tmp.path());
        let h = handle_with(
            Box::new(|r| anyhow::bail!("model_ref {r:?} not in registry")),
            profile_path.clone(),
        );

        let err = h
            .handle(Some(json!({"model_ref": "ghost"})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));

        let p: mur_common::agent::AgentProfile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path).unwrap()).unwrap();
        assert_eq!(p.model_ref.as_deref(), Some("old_ref"), "profile untouched");
    }

    #[tokio::test]
    async fn missing_model_ref_param_is_invalid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let h = handle_with(
            Box::new(|_r| Ok(Arc::new(FixedClient { name: "x" }) as _)),
            seed_profile(tmp.path()),
        );
        let err = h
            .handle(Some(json!({})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }
}
