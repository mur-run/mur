//! #2083: the registry key of the model that answered reaches `LlmResponse`
//! on both the chain path and the single-model path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::llm::{LlmClient, LlmError, LlmRequest, LlmResponse, StopReason};

fn named(model: &str) -> LlmResponse {
    LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        model: model.to_string(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
        registry_key: None,
    }
}

/// Answers every call as provider model `name`; counts calls.
struct Named(&'static str, AtomicUsize);

#[async_trait]
impl LlmClient for Named {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(named(self.0))
    }
    fn model_name(&self) -> &str {
        self.0
    }
}

fn retry0() -> RetryConfig {
    RetryConfig {
        max_retries: 0,
        backoff_base_ms: 1,
        cooldown_secs: 60,
    }
}

/// Every ref but `down` builds a client answering as `claude-sonnet-4`.
fn factory(down: &'static str) -> ClientFactory {
    Box::new(move |r: &str| {
        if r == down {
            Err(anyhow::anyhow!("{r} unavailable"))
        } else {
            Ok(Arc::new(Named("claude-sonnet-4", AtomicUsize::new(0))) as Arc<dyn LlmClient>)
        }
    })
}

/// The chain stamps the candidate that answered, whatever the provider client
/// calls its model — the provider name here is deliberately not a key.
#[tokio::test]
async fn the_answering_candidates_registry_key_is_stamped() {
    let fb = FallbackLlmClient::new(
        vec!["sonnet-sub".into(), "sonnet-api".into()],
        factory("sonnet-sub"),
        retry0(),
    );
    let resp = fb.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.model, "claude-sonnet-4");
    assert_eq!(resp.registry_key.as_deref(), Some("sonnet-api"));
}

/// The single-model wrapper stamps its key, but never over a more specific
/// one an inner chain already set.
#[tokio::test]
async fn the_keyed_wrapper_stamps_its_key_without_overriding_the_chains() {
    use crate::llm::keyed::KeyedLlmClient;
    let bare = KeyedLlmClient::wrap(
        Arc::new(Named("claude-opus-4", AtomicUsize::new(0))),
        "opus",
    );
    let resp = bare.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.registry_key.as_deref(), Some("opus"));

    let chain = FallbackLlmClient::new(vec!["b".into()], factory("none"), retry0());
    let wrapped = KeyedLlmClient::wrap(Arc::new(chain), "a");
    let resp = wrapped.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.registry_key.as_deref(), Some("b"));
}
