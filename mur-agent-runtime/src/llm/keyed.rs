//! Stamps a registry-backed client's `models.yaml` key on every response.
//!
//! A concrete provider client knows only the provider's model name
//! (`entry.model`), and that is not the registry key: an alias need not match
//! it, and two aliases can serve one name at different prices. The fallback
//! chain stamps its winning candidate itself; this wrapper does the same for
//! the single-model path, so `LlmResponse::registry_key` is set wherever the
//! key is known and spend accounting never has to guess it from a name (#2083).

use std::sync::Arc;

use async_trait::async_trait;

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StreamDelta};

pub struct KeyedLlmClient {
    inner: Arc<dyn LlmClient>,
    key: String,
}

impl KeyedLlmClient {
    pub fn wrap(inner: Arc<dyn LlmClient>, key: &str) -> Arc<dyn LlmClient> {
        Arc::new(Self {
            inner,
            key: key.to_string(),
        })
    }

    fn stamp(&self, r: Result<LlmResponse, LlmError>) -> Result<LlmResponse, LlmError> {
        r.map(|mut resp| {
            // An inner chain that already knows its winner is more specific.
            resp.registry_key.get_or_insert_with(|| self.key.clone());
            resp
        })
    }
}

#[async_trait]
impl LlmClient for KeyedLlmClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.stamp(self.inner.generate(req).await)
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        self.stamp(self.inner.generate_stream(req, sink).await)
    }
}
