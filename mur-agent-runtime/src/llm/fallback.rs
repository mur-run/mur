//! Failure-fallback for LLM calls: cooldown circuit-breaker, backoff, and token
//! estimate. See the model-switch spec.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{LlmRequest, RichMessage};

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
}
