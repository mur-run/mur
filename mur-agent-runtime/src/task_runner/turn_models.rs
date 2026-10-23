//! Per-model token usage of one turn (#2083).
//!
//! A turn is many LLM calls — the tool loop, plus a guard's closing summary —
//! and routing or fallback can answer each one with a different model. The
//! runner's cumulative counters fold all of them into one input and one output
//! figure; pricing that at any single model's rates reprices the others. This
//! ledger keeps each answering model's share apart so `Task.usage.by_model`
//! can be priced call-for-call by whoever reads it.

use std::sync::Mutex;

use crate::llm::LlmResponse;

/// One answering model's share of a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelShare {
    /// `models.yaml` key, when the client knew it (`LlmResponse::registry_key`).
    pub registry_key: Option<String>,
    /// The provider's model name (`LlmResponse::model`).
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Default)]
pub(crate) struct TurnModels {
    inner: Mutex<Ledger>,
}

#[derive(Debug, Default)]
struct Ledger {
    shares: Vec<ModelShare>,
    /// Provider name of the most recent call, whichever share it joined.
    last: Option<String>,
}

impl TurnModels {
    fn lock(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start a new turn: a stub backend that never calls a model reports no
    /// model rather than the previous turn's.
    pub fn reset(&self) {
        *self.lock() = Ledger::default();
    }

    /// Account one successful LLM call.
    pub fn record(&self, resp: &LlmResponse) {
        let mut ledger = self.lock();
        ledger.last = Some(resp.model.clone());
        let shares = &mut ledger.shares;
        let at = shares
            .iter()
            .position(|s| s.registry_key == resp.registry_key && s.model == resp.model);
        let share = match at {
            Some(i) => &mut shares[i],
            None => {
                shares.push(ModelShare {
                    registry_key: resp.registry_key.clone(),
                    model: resp.model.clone(),
                    input_tokens: 0,
                    output_tokens: 0,
                });
                shares.last_mut().expect("just pushed")
            }
        };
        share.input_tokens = share.input_tokens.saturating_add(resp.input_tokens);
        share.output_tokens = share.output_tokens.saturating_add(resp.output_tokens);
    }

    /// The provider name of the turn's most recent call — what
    /// `Task.usage.model_ref` has always carried.
    pub fn last_model(&self) -> Option<String> {
        self.lock().last.clone()
    }

    /// `Task.usage.by_model`: one entry per answering model, first-seen order.
    pub fn to_json(&self) -> serde_json::Value {
        self.lock()
            .shares
            .iter()
            .map(|s| {
                serde_json::json!({
                    "registry_key": s.registry_key,
                    "model": s.model,
                    "input_tokens": s.input_tokens,
                    "output_tokens": s.output_tokens,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::StopReason;

    fn resp(key: Option<&str>, model: &str, i: u64, o: u64) -> LlmResponse {
        LlmResponse {
            text: String::new(),
            input_tokens: i,
            output_tokens: o,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            model: model.into(),
            registry_key: key.map(str::to_string),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        }
    }

    #[test]
    fn keeps_each_answering_model_apart_and_sums_repeats() {
        let t = TurnModels::default();
        t.record(&resp(Some("opus"), "claude-opus-4", 1_000, 100));
        t.record(&resp(Some("local"), "qwen3", 500, 50));
        t.record(&resp(Some("opus"), "claude-opus-4", 2_000, 200));
        assert_eq!(
            t.last_model().as_deref(),
            Some("claude-opus-4"),
            "model_ref is the most recent call, not the most recent new model"
        );
        let j = t.to_json();
        assert_eq!(j.as_array().unwrap().len(), 2);
        assert_eq!(j[0]["registry_key"], "opus");
        assert_eq!(j[0]["input_tokens"], 3_000);
        assert_eq!(j[0]["output_tokens"], 300);
        assert_eq!(j[1]["registry_key"], "local");
        t.reset();
        assert!(t.last_model().is_none());
        assert_eq!(t.to_json(), serde_json::json!([]));
    }
}
