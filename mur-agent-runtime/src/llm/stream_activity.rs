//! Stream liveness: bound how long a model's stream may be SILENT, never how
//! long it may run (#1287).
//!
//! Its own module because it is the whole of that answer and `llm/mod.rs` is
//! the crate's LLM front door, which was already 618 lines before this and
//! would have gone past CLAUDE.md §4's 800 with it inlined.
//!
//! See `docs/superpowers/specs/2026-09-13-llm-idle-not-total-design.md` D3/D4.

use super::LlmError;

/// How long a stream may take to send its FIRST chunk.
///
/// This covers a cold local model: loading weights off disk and evaluating a
/// long prompt before emitting anything is a minutes-long operation, and it
/// looks identical to a hung endpoint from out here. Generous on purpose.
pub(crate) const LLM_FIRST_CHUNK_TIMEOUT_SECS: u64 = 300;

/// How long a stream that has already sent something may go quiet.
///
/// Tight on purpose: once a model is emitting, the gap between chunks is
/// sub-second, so two minutes of silence is a dead connection rather than a
/// slow one. Two separate bounds because one number cannot serve both ends —
/// it would have to be at least the cold-start figure, which makes a dead
/// mid-stream hang for five minutes and stops the bound meaning anything.
pub(crate) const LLM_STREAM_IDLE_TIMEOUT_SECS: u64 = 120;

/// Bounds how long a stream may be SILENT — never how long it may run.
///
/// This is the whole of #1287's answer. There is no ceiling on generation
/// anywhere: a body that is still arriving is live work. What is bounded is
/// the gap between arrivals, measured here rather than at the sink, because
/// tool-call argument fragments reach the sink as nothing at all — OpenAI
/// accumulates them into a partial map and Anthropic's `input_json_delta` arm
/// returns `None`. A guard watching the sink would see a model streaming a
/// large tool invocation as idle and abort it mid-call. Arrival of bytes is
/// the liveness signal, whatever those bytes turn out to mean.
pub(crate) struct StreamActivity {
    first: std::time::Duration,
    idle: std::time::Duration,
    seen_any: bool,
}

/// Read a seconds value from the environment, keeping `default` when it is
/// absent, unparseable, or zero.
///
/// Zero is rejected rather than honoured as "no bound": a bound that can be
/// switched off by a stray `=0` is a bound nobody can rely on, and the caller
/// asking for it is far more likely to have made a mistake than to want an
/// unbounded stream.
fn env_secs(var: &str, default: u64) -> std::time::Duration {
    match std::env::var(var) {
        Err(_) => std::time::Duration::from_secs(default),
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => std::time::Duration::from_secs(n),
            _ => {
                tracing::warn!(
                    var,
                    value = %raw,
                    default_secs = default,
                    "unusable value for an LLM stream idle bound; keeping the default \
                     (a typo must not disable the bound)"
                );
                std::time::Duration::from_secs(default)
            }
        },
    }
}

impl StreamActivity {
    pub(crate) fn from_env() -> Self {
        Self {
            first: env_secs(
                "MUR_LLM_FIRST_DELTA_TIMEOUT_SECS",
                LLM_FIRST_CHUNK_TIMEOUT_SECS,
            ),
            idle: env_secs("MUR_LLM_IDLE_TIMEOUT_SECS", LLM_STREAM_IDLE_TIMEOUT_SECS),
            seen_any: false,
        }
    }

    /// Await one chunk-producing future under the applicable bound.
    ///
    /// `Ok(None)` is end-of-stream and `Err(LlmError::Timeout)` is the bound
    /// elapsing; any other `Err` passes through untouched, because
    /// [`classify`] treats a transport failure and a timeout differently and
    /// relabelling one as the other would change how the fallback chain
    /// responds.
    ///
    /// Generic over the item: `Response::chunk` yields `bytes::Bytes`, `bytes`
    /// is not a direct dependency of this crate and reqwest does not re-export
    /// it, so the element type is left to inference at the three call sites.
    /// It also means the timing rule can be tested with a plain sleep instead
    /// of a socket.
    /// Fixed bounds for the timing tests, so they do not have to wait out the
    /// production five-minute cold-start figure even on a paused clock.
    #[cfg(test)]
    pub(crate) fn for_test(first_secs: u64, idle_secs: u64) -> Self {
        Self {
            first: std::time::Duration::from_secs(first_secs),
            idle: std::time::Duration::from_secs(idle_secs),
            seen_any: false,
        }
    }

    pub(crate) async fn bounded<T>(
        &mut self,
        fut: impl std::future::Future<Output = Result<Option<T>, LlmError>>,
    ) -> Result<Option<T>, LlmError> {
        let bound = if self.seen_any { self.idle } else { self.first };
        match tokio::time::timeout(bound, fut).await {
            Err(_) => Err(LlmError::Timeout),
            Ok(Err(e)) => Err(e),
            Ok(Ok(None)) => Ok(None),
            Ok(Ok(Some(v))) => {
                // Recorded HERE, before any caller looks at the bytes. That is
                // what makes an invisible SSE frame — a tool-argument
                // fragment, a usage update — count as life.
                self.seen_any = true;
                Ok(Some(v))
            }
        }
    }
}

#[cfg(test)]
mod stream_activity_tests {
    use super::*;
    use crate::llm::{Disposition, LlmError, classify};

    /// Helper: a future that "arrives" after `secs` with one item.
    async fn arrives_after(secs: u64) -> Result<Option<&'static str>, LlmError> {
        tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        Ok(Some("chunk"))
    }

    /// The two bounds are different bounds, and which one applies depends on
    /// whether anything has arrived yet. A single number could not express
    /// this: 90 s of silence is fine before the first byte and fatal after it.
    #[tokio::test(start_paused = true)]
    async fn the_first_bound_covers_cold_start_and_the_idle_bound_takes_over_after() {
        let mut a = StreamActivity::for_test(100, 10);
        // 50 s before anything has arrived: inside the FIRST bound.
        assert!(matches!(
            a.bounded(arrives_after(50)).await,
            Ok(Some("chunk"))
        ));
        // The same 50 s now that something has arrived: past the IDLE bound.
        assert!(matches!(
            a.bounded(arrives_after(50)).await,
            Err(LlmError::Timeout)
        ));
    }

    /// Nothing at all arrived, so nothing reached the sink and a retry cannot
    /// duplicate anything. `Timeout` is `RetryThenAdvance` in `classify`,
    /// which is exactly the disposition this case wants.
    #[tokio::test(start_paused = true)]
    async fn a_stream_that_never_starts_is_a_timeout() {
        let mut a = StreamActivity::for_test(10, 5);
        assert!(matches!(
            a.bounded(arrives_after(999)).await,
            Err(LlmError::Timeout)
        ));
        assert_eq!(classify(&LlmError::Timeout), Disposition::RetryThenAdvance);
    }

    /// End-of-stream must not be reported as a timeout. This is the difference
    /// between "the model finished" and "the model stopped sending".
    #[tokio::test(start_paused = true)]
    async fn end_of_stream_passes_through_as_none() {
        let mut a = StreamActivity::for_test(10, 5);
        let end: Result<Option<&str>, LlmError> = Ok(None);
        assert!(matches!(a.bounded(async { end }).await, Ok(None)));
    }

    /// A transport failure keeps its own class. `classify` sends `Connect` and
    /// `Timeout` down different paths, and mislabelling a refused connection
    /// as a timeout would change how the fallback chain responds to it.
    #[tokio::test(start_paused = true)]
    async fn an_inner_error_is_not_relabelled_a_timeout() {
        let mut a = StreamActivity::for_test(10, 5);
        let inner: Result<Option<&str>, LlmError> = Err(LlmError::Connect("refused".into()));
        assert!(matches!(
            a.bounded(async { inner }).await,
            Err(LlmError::Connect(_))
        ));
    }

    /// Arrival is the liveness signal, not what the bytes mean. The caller
    /// never gets a chance to say "that frame was only a tool-argument
    /// fragment, ignore it" — which is the bug a sink-side guard would have.
    #[tokio::test(start_paused = true)]
    async fn any_item_counts_as_activity_whatever_it_contains() {
        let mut a = StreamActivity::for_test(100, 10);
        // An empty chunk is still bytes off the socket.
        let empty: Result<Option<&str>, LlmError> = Ok(Some(""));
        assert!(matches!(a.bounded(async { empty }).await, Ok(Some(""))));
        assert!(a.seen_any, "an empty chunk must still count as life");
    }

    #[test]
    fn env_overrides_are_honoured() {
        // SAFETY: set and cleared within this test; nextest gives each test
        // its own process, so no sibling sees this.
        unsafe { std::env::set_var("MUR_LLM_IDLE_TIMEOUT_SECS", "7") };
        assert_eq!(
            env_secs("MUR_LLM_IDLE_TIMEOUT_SECS", 120),
            std::time::Duration::from_secs(7)
        );
        unsafe { std::env::remove_var("MUR_LLM_IDLE_TIMEOUT_SECS") };
    }

    /// A typo, or a zero, must not disable the bound. Zero is the dangerous
    /// one: read as "no bound" it would silently remove the protection.
    #[test]
    fn an_unusable_env_value_keeps_the_default() {
        for bad in ["", "abc", "0", "-5", "12.5"] {
            unsafe { std::env::set_var("MUR_LLM_IDLE_TIMEOUT_SECS", bad) };
            assert_eq!(
                env_secs("MUR_LLM_IDLE_TIMEOUT_SECS", 120),
                std::time::Duration::from_secs(120),
                "value {bad:?} must fall back to the default"
            );
        }
        unsafe { std::env::remove_var("MUR_LLM_IDLE_TIMEOUT_SECS") };
    }
}
