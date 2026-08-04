//! Shared `ChatBackendAdapter`: wraps `Arc<dyn ChatBackend>` to satisfy
//! `mur_common::llm::LlmClient`. Used by `mur skill generate` and
//! `mur agent wizard`.

use anyhow::Result;
use mur_common::config::Config;
use mur_common::error::LlmError;
use mur_common::llm::LlmClient;
use std::path::Path;
use std::sync::Arc;

use super::ChatBackend;

/// Thin adapter: wraps a `ChatBackend` to satisfy the `LlmClient` trait.
pub struct ChatBackendAdapter {
    pub(crate) backend: Arc<dyn ChatBackend>,
    pub(crate) model: String,
}

impl LlmClient for ChatBackendAdapter {
    fn complete(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
        let model = self.model.clone();
        let user = prompt.to_string();
        let sys = system.map(|s| s.to_string());
        let backend = self.backend.clone();
        async move {
            let req = crate::conversations::backend::ChatRequest {
                model: &model,
                system: sys.as_deref(),
                user: &user,
                max_tokens: 4096,
                temperature: Some(0.3),
                stop: vec![],
                cache_system: false,
                cache_user_prefix: None,
            };
            backend
                .generate(req)
                .await
                .map(|r| r.text)
                .map_err(|e| LlmError::Other(e.to_string()))
        }
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![])
    }
}

/// Build a `ChatBackendAdapter` from the user's config, with an optional
/// model override and a telemetry stage tag.
///
/// Recipe: load config → synthesize backend config → override model if given →
/// build the backend via factory → wrap in `ChatBackendAdapter`.
///
/// Returns the **concrete** `ChatBackendAdapter` (not `Arc<dyn LlmClient>`)
/// so callers can wrap it in whichever `Arc<dyn Trait>` they need.
pub fn build_chat_adapter(
    home: &Path,
    model_override: Option<&str>,
    stage: &'static str,
) -> anyhow::Result<ChatBackendAdapter> {
    let cfg = Config::load_or_default(&home.join("config.yaml"));
    let mut backend_cfg = cfg.conversations.ask.effective_backend(&cfg.llm);
    if let Some(m) = model_override {
        backend_cfg.model = m.to_string();
    }
    let backend: Arc<dyn ChatBackend> = super::factory::build_for_stage(&backend_cfg, stage)
        .map_err(|e| anyhow::anyhow!("build_chat_adapter: stage={stage} — {e:#}"))?;
    Ok(ChatBackendAdapter {
        backend,
        model: backend_cfg.model,
    })
}
