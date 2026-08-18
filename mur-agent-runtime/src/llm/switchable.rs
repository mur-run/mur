//! Hot-swappable LLM client — the runtime half of the murmur `/model` slash
//! command. Boot wraps the built client in this; the `model/set` A2A handler
//! replaces the inner client between turns, so a switch never needs a
//! supervisor restart.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, StreamDelta};

pub struct SwitchableLlmClient {
    inner: RwLock<Arc<dyn LlmClient>>,
    /// Static label, same precedent as `FallbackLlmClient::model_name`: the
    /// boot-time client's name, not the currently active one.
    boot_name: String,
}

impl SwitchableLlmClient {
    pub fn new(initial: Arc<dyn LlmClient>) -> Arc<Self> {
        let boot_name = initial.model_name().to_string();
        Arc::new(Self {
            inner: RwLock::new(initial),
            boot_name,
        })
    }

    /// Replace the client used from the next request on. In-flight requests
    /// hold their own `Arc` clone and finish on the old client.
    pub fn swap(&self, next: Arc<dyn LlmClient>) {
        *self.inner.write().expect("switchable client lock poisoned") = next;
    }

    /// Clone the current inner client. The lock is only held for the clone —
    /// never across an await.
    fn current(&self) -> Arc<dyn LlmClient> {
        self.inner
            .read()
            .expect("switchable client lock poisoned")
            .clone()
    }
}

#[async_trait]
impl LlmClient for SwitchableLlmClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.current().generate(req).await
    }

    fn model_name(&self) -> &str {
        &self.boot_name
    }

    async fn generate_stream(
        &self,
        req: LlmRequest,
        sink: tokio::sync::mpsc::Sender<StreamDelta>,
    ) -> Result<LlmResponse, LlmError> {
        self.current().generate_stream(req, sink).await
    }
}

/// Everything `model/set` needs: the live slot to swap plus a builder that
/// turns a registry ref into a concrete client (the boot single-model path).
/// Only produced on that path — chain/routing and echo agents get `None`, so
/// the method is simply not registered for them.
pub struct ModelSwitchHandle {
    pub switchable: Arc<SwitchableLlmClient>,
    pub build_client: super::fallback::ClientFactory,
}

#[cfg(test)]
mod tests {
    use super::super::{RequestIntent, StopReason};
    use super::*;

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

    #[tokio::test]
    async fn swap_is_visible_to_the_next_request() {
        let sw = SwitchableLlmClient::new(Arc::new(FixedClient { name: "before" }));
        assert_eq!(sw.generate(req()).await.unwrap().text, "before");

        sw.swap(Arc::new(FixedClient { name: "after" }));
        assert_eq!(sw.generate(req()).await.unwrap().text, "after");
        // Label stays the boot-time name by design (FallbackLlmClient precedent).
        assert_eq!(sw.model_name(), "before");
    }

    #[tokio::test]
    async fn in_flight_clone_keeps_the_old_client() {
        let sw = SwitchableLlmClient::new(Arc::new(FixedClient { name: "old" }));
        let held = sw.current();
        sw.swap(Arc::new(FixedClient { name: "new" }));
        assert_eq!(held.generate(req()).await.unwrap().text, "old");
        assert_eq!(sw.generate(req()).await.unwrap().text, "new");
    }
}
