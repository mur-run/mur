//! ChatBackend decorator that adds retry-with-linear-backoff.
//!
//! Lifts the retry shape from mur-core/src/extract_llm.rs:215-260 but
//! dispatches on typed BackendError variants instead of string matching.
//!
//! Composable: factory::build wraps Anthropic + Ollama backends, but
//! tests can build MockBackend without retries (or wrap it manually).
//!
//! See spec §8.1.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::Stream;

use super::{BackendError, ChatBackend, ChatChunk, ChatRequest, ChatResponse};

/// Retry policy. P1 uses fixed defaults; future phases may make it configurable.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum total attempts (including the first). Default 3 = 1 try + 2 retries.
    pub max_attempts: u32,
    /// Base backoff in seconds — actual sleep = base * (attempt+1).
    /// Linear backoff for P1; exponential is overkill for our 3-attempt window.
    pub base_backoff_secs: u64,
    /// Cap on retry-after honoring (for RateLimited).
    pub max_retry_after_secs: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff_secs: 2,
            max_retry_after_secs: 30,
        }
    }
}

pub struct RetryingBackend {
    inner: Arc<dyn ChatBackend>,
    policy: RetryPolicy,
}

impl RetryingBackend {
    pub fn new(inner: Arc<dyn ChatBackend>, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Convenience: wrap with default policy.
    pub fn with_default_policy(inner: Arc<dyn ChatBackend>) -> Self {
        Self::new(inner, RetryPolicy::default())
    }

    /// Returns Some(sleep_duration) if we should retry, None if not.
    /// Splits the policy decision out so tests can assert it directly.
    fn should_retry(err: &anyhow::Error, attempt: u32, policy: &RetryPolicy) -> Option<Duration> {
        if attempt + 1 >= policy.max_attempts {
            return None;
        }
        let typed = err.downcast_ref::<BackendError>()?;
        let base = Duration::from_secs(policy.base_backoff_secs * (attempt + 1) as u64);
        match typed {
            BackendError::Timeout { .. } => Some(base),
            BackendError::ServerError { status, .. } if (500..=599).contains(status) => Some(base),
            BackendError::RateLimited {
                retry_after_secs, ..
            } => {
                let after = retry_after_secs
                    .map(|s| s.min(policy.max_retry_after_secs))
                    .unwrap_or(policy.base_backoff_secs);
                Some(Duration::from_secs(after))
            }
            // reqwest timeouts and connect failures land in BackendError::Network
            // (not BackendError::Timeout — that variant is only constructed in
            // tests). Pre-P4 extract_llm matched on the substring "timeout" in
            // the error string; restoring parity here so transient connect/read
            // timeouts still get the retry envelope rather than silently
            // degrading to logic-only fallback.
            BackendError::Network { source, .. } => {
                if source.is_timeout() || source.is_connect() {
                    Some(base)
                } else {
                    None
                }
            }
            // Non-retryable: Unauthorized, ModelNotFound, BadResponse,
            // Network{ !is_timeout && !is_connect } (e.g. TLS / decode errors)
            _ => None,
        }
    }
}

#[async_trait]
impl ChatBackend for RetryingBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let mut attempt: u32 = 0;
        loop {
            // ChatRequest's borrowed fields stay alive for the loop;
            // clone the owned `stop` Vec so each call gets a fresh one.
            let req_clone = ChatRequest {
                model: req.model,
                system: req.system,
                user: req.user,
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                stop: req.stop.clone(),
                cache_system: req.cache_system,
                cache_user_prefix: req.cache_user_prefix,
            };
            match self.inner.generate(req_clone).await {
                Ok(resp) => return Ok(resp),
                Err(e) => match Self::should_retry(&e, attempt, &self.policy) {
                    Some(delay) => {
                        tracing::warn!(
                            provider = self.inner.provider_name(),
                            attempt = attempt + 1,
                            max_attempts = self.policy.max_attempts,
                            delay_secs = delay.as_secs(),
                            "backend transient error: {e:#}, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    None => return Err(e),
                },
            }
        }
    }

    async fn generate_stream(
        &self,
        req: ChatRequest<'_>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatChunk>> + Send>>> {
        // Retry the connect attempt only — mid-stream failures propagate.
        // Mid-stream retry would re-send the prompt and silently waste tokens
        // on a duplicate request. P3+ may revisit if telemetry shows this is
        // a real problem.
        let mut attempt: u32 = 0;
        loop {
            let req_clone = ChatRequest {
                model: req.model,
                system: req.system,
                user: req.user,
                max_tokens: req.max_tokens,
                temperature: req.temperature,
                stop: req.stop.clone(),
                cache_system: req.cache_system,
                cache_user_prefix: req.cache_user_prefix,
            };
            match self.inner.generate_stream(req_clone).await {
                Ok(stream) => return Ok(stream),
                Err(e) => match Self::should_retry(&e, attempt, &self.policy) {
                    Some(delay) => {
                        tracing::warn!(
                            provider = self.inner.provider_name(),
                            attempt = attempt + 1,
                            max_attempts = self.policy.max_attempts,
                            delay_secs = delay.as_secs(),
                            "backend stream connect transient error: {e:#}, retrying"
                        );
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                    None => return Err(e),
                },
            }
        }
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    fn supports_caching(&self) -> bool {
        self.inner.supports_caching()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::backend::{ChatStream, Usage};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Test backend that fails N times then returns success. Uses an
    /// atomic counter so retries are deterministic regardless of policy.
    struct FailNTimes {
        fail_n: u32,
        attempts: Arc<AtomicU32>,
        err_factory: fn() -> BackendError,
    }

    impl FailNTimes {
        fn new(fail_n: u32, err_factory: fn() -> BackendError) -> Self {
            Self {
                fail_n,
                attempts: Arc::new(AtomicU32::new(0)),
                err_factory,
            }
        }
    }

    #[async_trait]
    impl ChatBackend for FailNTimes {
        async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                return Err((self.err_factory)().into());
            }
            Ok(ChatResponse {
                text: "ok".into(),
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    provider: "test",
                    model: req.model.into(),
                },
            })
        }

        async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
            anyhow::bail!("not used in retry tests")
        }

        fn provider_name(&self) -> &'static str {
            "test"
        }
    }

    fn req<'a>() -> ChatRequest<'a> {
        ChatRequest {
            model: "x",
            system: None,
            user: "p",
            max_tokens: 1,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    fn fast_policy() -> RetryPolicy {
        // Sub-second backoff so tests don't actually wait.
        RetryPolicy {
            max_attempts: 3,
            base_backoff_secs: 0,
            max_retry_after_secs: 0,
        }
    }

    #[tokio::test]
    async fn retries_on_500_then_succeeds() {
        let inner = Arc::new(FailNTimes::new(2, || BackendError::ServerError {
            provider: "test",
            status: 500,
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let resp = backend.generate(req()).await.unwrap();
        assert_eq!(resp.text, "ok");
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // 1 + 2 retries
    }

    #[tokio::test]
    async fn retries_on_timeout() {
        let inner = Arc::new(FailNTimes::new(1, || BackendError::Timeout {
            provider: "test",
            seconds: 30,
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let _ = backend.generate(req()).await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2); // 1 + 1 retry
    }

    #[tokio::test]
    async fn does_not_retry_on_unauthorized() {
        let inner = Arc::new(FailNTimes::new(99, || BackendError::Unauthorized {
            provider: "test",
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate(req()).await;
        assert!(r.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1); // No retries
    }

    #[tokio::test]
    async fn does_not_retry_on_model_not_found() {
        let inner = Arc::new(FailNTimes::new(99, || BackendError::ModelNotFound {
            provider: "test",
            model: "fake".into(),
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let _ = backend.generate(req()).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let inner = Arc::new(FailNTimes::new(99, || BackendError::ServerError {
            provider: "test",
            status: 503,
        }));
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate(req()).await;
        assert!(r.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // max_attempts = 3
    }

    #[tokio::test]
    async fn rate_limited_honors_retry_after_capped() {
        let inner = Arc::new(FailNTimes::new(1, || BackendError::RateLimited {
            provider: "test",
            retry_after_secs: Some(99),
        }));
        let attempts = inner.attempts.clone();
        // Cap at 0s so the test doesn't actually wait, but verify the dispatch path.
        let policy = RetryPolicy {
            max_attempts: 3,
            base_backoff_secs: 0,
            max_retry_after_secs: 0,
        };
        let backend = RetryingBackend::new(inner, policy);
        let _ = backend.generate(req()).await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn generate_stream_retries_connect_then_succeeds() {
        // Inner backend: fails generate_stream twice with ServerError(503),
        // then succeeds with a single-chunk stream.
        struct StreamFailNTimes {
            fail_n: u32,
            attempts: Arc<AtomicU32>,
        }
        #[async_trait]
        impl ChatBackend for StreamFailNTimes {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("not used")
            }
            async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
                let n = self.attempts.fetch_add(1, Ordering::SeqCst);
                if n < self.fail_n {
                    return Err(BackendError::ServerError {
                        provider: "test",
                        status: 503,
                    }
                    .into());
                }
                let chunk = ChatChunk {
                    delta: "hi".into(),
                    usage: Some(Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        provider: "test",
                        model: req.model.into(),
                    }),
                };
                Ok(Box::pin(futures::stream::iter(vec![Ok(chunk)])))
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }
        let inner = Arc::new(StreamFailNTimes {
            fail_n: 2,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        use futures::StreamExt;
        let mut stream = backend.generate_stream(req()).await.unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.delta, "hi");
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // 1 + 2 retries
    }

    /// Fixture that produces a REAL reqwest timeout error per call by hitting
    /// a wiremock endpoint that deliberately delays past a tight client
    /// timeout. This is the only reliable way to get an honest
    /// `reqwest::Error` whose `is_timeout()` is true — the type can't be
    /// hand-constructed from outside the crate. Each call increments
    /// `attempts` so we can assert the retry envelope ran.
    struct TimingOutNetwork {
        server: wiremock::MockServer,
        client: reqwest::Client,
        attempts: Arc<AtomicU32>,
        max_real_attempts: u32,
    }

    #[async_trait]
    impl ChatBackend for TimingOutNetwork {
        async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
            let n = self.attempts.fetch_add(1, Ordering::SeqCst);
            if n < self.max_real_attempts {
                // Make a real reqwest call that will time out (server delays
                // 500ms, client timeout is 50ms). The resulting reqwest::Error
                // has is_timeout() == true, exactly like a transient cloud-LLM
                // timeout would produce in OpenAIBackend / GeminiBackend /
                // AnthropicBackend.
                let url = format!("{}/api/slow", self.server.uri());
                let err = self
                    .client
                    .get(&url)
                    .send()
                    .await
                    .expect_err("the wiremock delay must exceed the client timeout");
                debug_assert!(
                    err.is_timeout() || err.is_connect(),
                    "fixture invariant: wanted a timeout/connect error, got {err:?}"
                );
                return Err(BackendError::Network {
                    provider: "test",
                    source: err,
                }
                .into());
            }
            // Final attempt succeeds.
            Ok(ChatResponse {
                text: "ok".into(),
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    provider: "test",
                    model: "x".into(),
                },
            })
        }

        async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
            anyhow::bail!("not used in retry tests")
        }

        fn provider_name(&self) -> &'static str {
            "test"
        }
    }

    #[tokio::test]
    async fn retries_on_network_timeout_then_succeeds() {
        // Regression test for the I1 follow-up: pre-P4 extract_llm retried
        // on the substring "timeout" in the error string. Post-P4, reqwest
        // timeouts land in BackendError::Network (NOT BackendError::Timeout —
        // that variant is only constructed in tests). Ensure should_retry
        // honors them when the underlying reqwest::Error reports is_timeout()
        // or is_connect(), so transient cloud-LLM blips still get retried
        // rather than silently degrading to logic-only fallback.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(500)))
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .unwrap();
        let inner = Arc::new(TimingOutNetwork {
            server,
            client,
            attempts: Arc::new(AtomicU32::new(0)),
            max_real_attempts: 2, // fail twice, succeed on attempt 3
        });
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let resp = backend.generate(req()).await.unwrap();
        assert_eq!(resp.text, "ok");
        // 1 initial + 2 retries = 3 calls. If should_retry didn't cover
        // Network{is_timeout()}, this would be 1 (immediate bail).
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_on_network_decode_error() {
        // Counterpart to the timeout case: a Network error whose underlying
        // reqwest::Error is NOT a timeout/connect failure must NOT retry.
        // We synthesize this by using a real "decode" reqwest error (server
        // returns a body that can't be parsed as the expected type via
        // resp.json::<T>(), producing reqwest::Error::is_decode() == true).
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/junk"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("not-json{"),
            )
            .mount(&server)
            .await;

        struct DecodeFailingNetwork {
            server: wiremock::MockServer,
            client: reqwest::Client,
            attempts: Arc<AtomicU32>,
        }
        #[async_trait]
        impl ChatBackend for DecodeFailingNetwork {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                let url = format!("{}/api/junk", self.server.uri());
                // resp.json::<Value>() will produce reqwest::Error with
                // is_decode() == true, is_timeout() == false, is_connect() == false.
                let err = self
                    .client
                    .get(&url)
                    .send()
                    .await
                    .unwrap()
                    .json::<serde_json::Value>()
                    .await
                    .expect_err("malformed body must fail to decode");
                debug_assert!(
                    !err.is_timeout() && !err.is_connect(),
                    "fixture invariant: wanted a non-timeout reqwest error, got {err:?}"
                );
                Err(BackendError::Network {
                    provider: "test",
                    source: err,
                }
                .into())
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                anyhow::bail!("not used")
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let inner = Arc::new(DecodeFailingNetwork {
            server,
            client,
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate(req()).await;
        assert!(r.is_err());
        // Exactly 1 call — no retries on non-transient Network errors.
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn generate_stream_does_not_retry_on_unauthorized_at_connect() {
        struct AlwaysUnauthorized {
            attempts: Arc<AtomicU32>,
        }
        #[async_trait]
        impl ChatBackend for AlwaysUnauthorized {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("not used")
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                self.attempts.fetch_add(1, Ordering::SeqCst);
                Err(BackendError::Unauthorized { provider: "test" }.into())
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }
        let inner = Arc::new(AlwaysUnauthorized {
            attempts: Arc::new(AtomicU32::new(0)),
        });
        let attempts = inner.attempts.clone();
        let backend = RetryingBackend::new(inner, fast_policy());
        let r = backend.generate_stream(req()).await;
        assert!(r.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1); // No retries
    }
}
