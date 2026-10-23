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

/// Runner-lifetime, per-answering-model token counters. Never reset: a turn
/// takes a [`TurnMark`] when it starts and reports the difference when it
/// ends, exactly like the runner's `cumulative_*_tokens` snapshot-delta.
///
/// Why not a per-turn ledger reset at turn start: two turns can overlap on one
/// runner (a router plan may hand one member two parallel steps; a user can
/// chat with a member mid-run), and a reset by the second turn would erase the
/// first turn's in-flight calls — an under-count, the dangerous direction for a
/// spend cap. A snapshot delta can only over-count under overlap (each turn
/// also sees the other's calls), which a cap tolerates.
#[derive(Debug, Default)]
pub(crate) struct TurnModels {
    inner: Mutex<Ledger>,
}

#[derive(Debug, Default)]
struct Ledger {
    shares: Vec<ModelShare>,
    /// Calls recorded so far (runner lifetime).
    calls: u64,
    /// Provider name of the most recent call, whichever share it joined.
    last: Option<String>,
}

/// Where a turn started: the cumulative shares and call count at that moment.
#[derive(Debug, Clone, Default)]
pub(crate) struct TurnMark {
    shares: Vec<ModelShare>,
    calls: u64,
}

impl TurnModels {
    fn lock(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Mark the start of a turn; pass the mark to [`Self::last_model_since`]
    /// and [`Self::to_json_since`] when the turn ends.
    pub fn mark(&self) -> TurnMark {
        let ledger = self.lock();
        TurnMark {
            shares: ledger.shares.clone(),
            calls: ledger.calls,
        }
    }

    /// Account one successful LLM call.
    pub fn record(&self, resp: &LlmResponse) {
        let mut ledger = self.lock();
        ledger.calls = ledger.calls.saturating_add(1);
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

    /// The provider name of the most recent call since `mark` — what
    /// `Task.usage.model_ref` has always carried. `None` when no call landed
    /// since the mark (a stub backend reports no model, not a stale one).
    pub fn last_model_since(&self, mark: &TurnMark) -> Option<String> {
        let ledger = self.lock();
        if ledger.calls > mark.calls {
            ledger.last.clone()
        } else {
            None
        }
    }

    /// `Task.usage.by_model`: each answering model's usage since `mark`, one
    /// entry per model in first-seen order; models with no usage since the
    /// mark are left out.
    pub fn to_json_since(&self, mark: &TurnMark) -> serde_json::Value {
        self.lock()
            .shares
            .iter()
            .filter_map(|s| {
                let before = mark
                    .shares
                    .iter()
                    .find(|b| b.registry_key == s.registry_key && b.model == s.model);
                let (i0, o0) = before.map_or((0, 0), |b| (b.input_tokens, b.output_tokens));
                let input_tokens = s.input_tokens.saturating_sub(i0);
                let output_tokens = s.output_tokens.saturating_sub(o0);
                (input_tokens > 0 || output_tokens > 0).then(|| {
                    serde_json::json!({
                        "registry_key": s.registry_key,
                        "model": s.model,
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                    })
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
        let m = t.mark();
        t.record(&resp(Some("opus"), "claude-opus-4", 1_000, 100));
        t.record(&resp(Some("local"), "qwen3", 500, 50));
        t.record(&resp(Some("opus"), "claude-opus-4", 2_000, 200));
        assert_eq!(
            t.last_model_since(&m).as_deref(),
            Some("claude-opus-4"),
            "model_ref is the most recent call, not the most recent new model"
        );
        let j = t.to_json_since(&m);
        assert_eq!(j.as_array().unwrap().len(), 2);
        assert_eq!(j[0]["registry_key"], "opus");
        assert_eq!(j[0]["input_tokens"], 3_000);
        assert_eq!(j[0]["output_tokens"], 300);
        assert_eq!(j[1]["registry_key"], "local");
    }

    #[test]
    fn a_later_turn_reports_only_its_own_calls() {
        let t = TurnModels::default();
        t.record(&resp(Some("opus"), "claude-opus-4", 1_000, 100));
        let m = t.mark();
        assert!(
            t.last_model_since(&m).is_none(),
            "a turn with no calls reports no model, not the previous turn's"
        );
        assert_eq!(t.to_json_since(&m), serde_json::json!([]));
        t.record(&resp(Some("opus"), "claude-opus-4", 40, 4));
        let j = t.to_json_since(&m);
        assert_eq!(j.as_array().unwrap().len(), 1);
        assert_eq!(j[0]["input_tokens"], 40);
        assert_eq!(j[0]["output_tokens"], 4);
    }

    /// #2083 review: turn B starting while turn A is mid-flight on the same
    /// runner must not erase A's calls. Overlap may over-count (each turn
    /// sees the other's calls) but never drops one.
    #[test]
    fn overlapping_turns_never_lose_a_call() {
        let t = TurnModels::default();
        let a = t.mark();
        t.record(&resp(Some("opus"), "claude-opus-4", 1_000, 100)); // a1
        let b = t.mark(); // B starts while A is still running
        t.record(&resp(Some("local"), "qwen3", 500, 50)); // b1
        t.record(&resp(Some("opus"), "claude-opus-4", 2_000, 200)); // a2

        let ja = t.to_json_since(&a);
        let jb = t.to_json_since(&b);
        let sum = |j: &serde_json::Value, key: &str| -> u64 {
            j.as_array()
                .unwrap()
                .iter()
                .filter(|s| s["registry_key"] == key)
                .map(|s| s["input_tokens"].as_u64().unwrap())
                .sum()
        };
        assert_eq!(sum(&ja, "opus"), 3_000, "A keeps a1 and a2");
        assert_eq!(sum(&ja, "local"), 500, "A over-counts B's call: safe");
        assert_eq!(sum(&jb, "opus"), 2_000);
        assert_eq!(sum(&jb, "local"), 500);
        assert!(
            sum(&ja, "opus") + sum(&jb, "opus") >= 3_000,
            "no opus call is missing from the union of both turns"
        );
    }
}
