//! Failure-fallback for LLM calls: cooldown circuit-breaker, backoff, and token
//! estimate. See the model-switch spec.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mur_common::config::RetryConfig;

use super::{LlmClient, LlmError, LlmRequest, LlmResponse, Retryability, RichMessage, classify};

/// Factory that builds a concrete `LlmClient` for a given model_ref. Boxed so
/// `FallbackLlmClient` doesn't need a generic parameter per candidate type.
pub type ClientFactory = Box<dyn Fn(&str) -> anyhow::Result<Arc<dyn LlmClient>> + Send + Sync>;

/// An `LlmClient` that tries an ordered list of model_refs, advancing on
/// retryable failures (per-candidate backoff retries first), skipping models in
/// cooldown, and returning a fatal error immediately. Drops into the existing
/// `Arc<dyn LlmClient>` slot with no agent-loop change.
pub struct FallbackLlmClient {
    candidates: Vec<String>,
    factory: ClientFactory,
    retry: RetryConfig,
    cooldown: CooldownMap,
    primary_name: String,
}

impl FallbackLlmClient {
    pub fn new(candidates: Vec<String>, factory: ClientFactory, retry: RetryConfig) -> Self {
        let primary_name = candidates.first().cloned().unwrap_or_default();
        Self {
            candidates,
            factory,
            retry,
            cooldown: CooldownMap::new(),
            primary_name,
        }
    }
}

#[async_trait]
impl LlmClient for FallbackLlmClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let now = Instant::now();
        let mut last: Option<LlmError> = None;
        for model_ref in &self.candidates {
            if self.cooldown.is_cooling(model_ref, now) {
                continue;
            }
            let client = match (self.factory)(model_ref) {
                Ok(c) => c,
                Err(e) => {
                    last = Some(LlmError::Http(format!("build {model_ref}: {e}")));
                    continue;
                }
            };
            for attempt in 0..=self.retry.max_retries {
                match client.generate(req.clone()).await {
                    Ok(resp) => return Ok(resp),
                    Err(e) => match classify(&e) {
                        Retryability::Fatal => return Err(e),
                        Retryability::Retryable => {
                            tracing::info!(model_ref, attempt, error = %e, "llm fallback: retryable failure");
                            if attempt < self.retry.max_retries {
                                tokio::time::sleep(backoff_delay(
                                    attempt,
                                    self.retry.backoff_base_ms,
                                ))
                                .await;
                            } else {
                                let until =
                                    Instant::now() + Duration::from_secs(self.retry.cooldown_secs);
                                self.cooldown.mark(model_ref, until);
                                tracing::info!(
                                    model_ref,
                                    "llm fallback: cooling down, advancing chain"
                                );
                                last = Some(e);
                            }
                        }
                    },
                }
            }
        }
        Err(last.unwrap_or_else(|| LlmError::InvalidResponse("no model candidates".into())))
    }

    fn model_name(&self) -> &str {
        &self.primary_name
    }
}

/// In-memory per-model cooldown (circuit-breaker). Process-local; a restart
/// clears it (cooldowns are seconds-scale, so persistence is unnecessary).
#[derive(Default)]
pub struct CooldownMap {
    inner: Mutex<HashMap<String, Instant>>,
}

impl CooldownMap {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn mark(&self, model_ref: &str, until: Instant) {
        self.inner
            .lock()
            .unwrap()
            .insert(model_ref.to_string(), until);
    }

    pub fn is_cooling(&self, model_ref: &str, now: Instant) -> bool {
        match self.inner.lock().unwrap().get(model_ref) {
            Some(until) => now < *until,
            None => false,
        }
    }
}

/// Exponential backoff with jitter: `base * 2^attempt + rand[0, base)`.
pub fn backoff_delay(attempt: u32, base_ms: u64) -> Duration {
    let floor = base_ms.saturating_mul(2u64.saturating_pow(attempt));
    let jitter = if base_ms > 0 {
        rand::random::<u64>() % base_ms
    } else {
        0
    };
    Duration::from_millis(floor.saturating_add(jitter))
}

/// Coarse input-token estimate (chars/4) over the request's message texts —
/// enough for a routing heuristic, not billing. `LlmRequest.messages` is a
/// `Vec<RichMessage>` (an enum), so match the text-bearing variants.
pub fn estimate_input_tokens(req: &LlmRequest) -> u32 {
    let chars: usize = req
        .messages
        .iter()
        .map(|m| match m {
            RichMessage::Text { content, .. } => content.len(),
            RichMessage::ImageText { text, .. } => text.len(),
            RichMessage::ToolUse { text, .. } => text.as_deref().map_or(0, str::len),
            RichMessage::ToolResults { .. } => 0,
        })
        .sum();
    (chars / 4).min(u32::MAX as usize) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_marks_and_expires() {
        let cm = CooldownMap::new();
        let now = Instant::now();
        assert!(!cm.is_cooling("m", now));
        cm.mark("m", now + Duration::from_secs(60));
        assert!(cm.is_cooling("m", now)); // within window
        assert!(!cm.is_cooling("m", now + Duration::from_secs(61))); // after window
        assert!(!cm.is_cooling("other", now));
    }

    #[test]
    fn backoff_grows_and_stays_in_bounds() {
        let base = 500u64;
        for attempt in 0..4u32 {
            let d = backoff_delay(attempt, base).as_millis() as u64;
            let floor = base * 2u64.pow(attempt);
            assert!(
                d >= floor && d < floor + base,
                "attempt {attempt}: {d} not in [{floor}, {})",
                floor + base
            );
        }
    }

    #[test]
    fn estimate_tokens_sums_text_over_rich_messages() {
        let req = LlmRequest {
            messages: vec![
                RichMessage::Text {
                    role: "user".into(),
                    content: "a".repeat(40),
                },
                RichMessage::ImageText {
                    role: "user".into(),
                    media_type: "image/png".into(),
                    data: String::new(),
                    text: "b".repeat(40),
                },
            ],
            temperature: None,
            max_tokens: None,
            tools: vec![],
        };
        assert_eq!(estimate_input_tokens(&req), 20); // 80 chars / 4
    }

    use super::super::StopReason;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // LlmResponse has no Default (StopReason has no default variant), so build
    // one explicitly.
    fn mk_resp(text: &str) -> LlmResponse {
        LlmResponse {
            text: text.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model: text.to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        }
    }

    // Mock client whose Nth generate() outcome is scripted.
    struct ScriptClient {
        name: String,
        outcomes: Vec<Result<(), LlmError>>,
        idx: AtomicUsize,
    }
    #[async_trait]
    impl LlmClient for ScriptClient {
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let i = self
                .idx
                .fetch_add(1, Ordering::SeqCst)
                .min(self.outcomes.len() - 1);
            match &self.outcomes[i] {
                Ok(()) => Ok(mk_resp(&self.name)),
                Err(e) => Err(e.clone()),
            }
        }
        fn model_name(&self) -> &str {
            &self.name
        }
    }

    fn factory_for(scripts: HashMap<String, Vec<Result<(), LlmError>>>) -> ClientFactory {
        Box::new(move |r: &str| {
            let o = scripts.get(r).cloned().unwrap_or_else(|| vec![Ok(())]);
            Ok(Arc::new(ScriptClient {
                name: r.to_string(),
                outcomes: o,
                idx: AtomicUsize::new(0),
            }) as Arc<dyn LlmClient>)
        })
    }

    fn retry0() -> RetryConfig {
        RetryConfig {
            max_retries: 0,
            backoff_base_ms: 1,
            cooldown_secs: 60,
        }
    }

    #[tokio::test]
    async fn advances_chain_on_retryable_then_succeeds() {
        let mut s = HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::ServerError(500))]); // a fails (retryable)
        s.insert("b".into(), vec![Ok(())]); // b succeeds
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let resp = fb.generate(LlmRequest::default()).await.unwrap();
        assert_eq!(resp.text, "b"); // fell through to b
    }

    #[tokio::test]
    async fn fatal_error_does_not_advance() {
        let mut s = HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::Http("401".into()))]); // fatal
        s.insert("b".into(), vec![Ok(())]);
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Http(_))); // returned a's fatal error, never tried b
    }

    #[tokio::test]
    async fn exhaustion_returns_last_error() {
        let mut s = HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::RateLimit)]);
        s.insert("b".into(), vec![Err(LlmError::ServerError(503))]);
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::ServerError(503))); // last candidate's error
    }
}
