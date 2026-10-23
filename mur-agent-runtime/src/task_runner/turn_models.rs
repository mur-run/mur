//! Per-model token usage of one turn (#2083).
//!
//! A turn is many LLM calls — the tool loop, plus a guard's closing summary —
//! and routing or fallback can answer each one with a different model. The
//! runner's cumulative counters fold all of them into one input and one output
//! figure; pricing that at any single model's rates reprices the others. This
//! ledger keeps each answering model's share apart so `Task.usage.by_model`
//! can be priced call-for-call by whoever reads it.

use std::future::Future;
use std::sync::{Arc, Mutex};

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

/// One turn's per-answering-model usage ledger.
///
/// Owned by exactly one turn: `run_sync_inner` creates it, runs the turn's
/// generation inside [`scope`], and every LLM call made in that future lands
/// here through [`record`]. A runner-wide ledger cannot do this — overlapping
/// turns on one runner (two parallel plan steps on one member, a user chat
/// mid-run) would either erase each other's calls (reset at turn start) or
/// count each other's calls (snapshot delta), and the fleet guard sums every
/// turn's report, so both directions misstate actual spend (#2083).
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

tokio::task_local! {
    /// The ledger of the turn whose generation future is being polled.
    static CURRENT: Arc<TurnModels>;
}

/// Run `fut` (one turn's generation) with `ledger` as the ledger every LLM
/// call inside it records into. The caller keeps its own `Arc`, so the
/// ledger stays readable after a cancelled turn drops `fut`.
pub(crate) async fn scope<F: Future>(ledger: Arc<TurnModels>, fut: F) -> F::Output {
    CURRENT.scope(ledger, fut).await
}

/// Account one successful LLM call to the turn it was made in. A call made
/// outside any turn scope belongs to no turn and is not reported by any.
pub(crate) fn record(resp: &LlmResponse) {
    if CURRENT.try_with(|ledger| ledger.record(resp)).is_err() {
        tracing::debug!(model = %resp.model, "LLM call outside a turn scope; not in any Task.usage");
    }
}

impl TurnModels {
    fn lock(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, resp: &LlmResponse) {
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

    /// The provider name of this turn's most recent call — what
    /// `Task.usage.model_ref` has always carried. `None` when the turn made
    /// no call (a stub backend reports no model, not a stale one).
    pub fn last_model(&self) -> Option<String> {
        self.lock().last.clone()
    }

    /// This turn's `(input, output)` totals: the sum of its shares.
    pub fn totals(&self) -> (u64, u64) {
        self.lock().shares.iter().fold((0, 0), |(i, o), s| {
            (
                i.saturating_add(s.input_tokens),
                o.saturating_add(s.output_tokens),
            )
        })
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

    fn ledger() -> Arc<TurnModels> {
        Arc::new(TurnModels::default())
    }

    #[tokio::test]
    async fn keeps_each_answering_model_apart_and_sums_repeats() {
        let t = ledger();
        scope(t.clone(), async {
            record(&resp(Some("opus"), "claude-opus-4", 1_000, 100));
            record(&resp(Some("local"), "qwen3", 500, 50));
            record(&resp(Some("opus"), "claude-opus-4", 2_000, 200));
        })
        .await;
        assert_eq!(
            t.last_model().as_deref(),
            Some("claude-opus-4"),
            "model_ref is the most recent call, not the most recent new model"
        );
        assert_eq!(t.totals(), (3_500, 350));
        let j = t.to_json();
        assert_eq!(j.as_array().unwrap().len(), 2);
        assert_eq!(j[0]["registry_key"], "opus");
        assert_eq!(j[0]["input_tokens"], 3_000);
        assert_eq!(j[0]["output_tokens"], 300);
        assert_eq!(j[1]["registry_key"], "local");
    }

    #[tokio::test]
    async fn a_turn_with_no_calls_reports_nothing() {
        let t = ledger();
        scope(t.clone(), async {}).await;
        assert!(t.last_model().is_none(), "no model, not a stale one");
        assert_eq!(t.totals(), (0, 0));
        assert_eq!(t.to_json(), serde_json::json!([]));
    }

    /// #2083 review: two turns interleaving on one runner each report exactly
    /// their own calls — no call lost, none counted twice — and a call made
    /// outside any turn (unrelated work) is charged to neither.
    #[tokio::test]
    async fn overlapping_turns_count_each_call_exactly_once() {
        let (a, b) = (ledger(), ledger());
        let (a_go, a_wait) = tokio::sync::oneshot::channel::<()>();
        let (b_go, b_wait) = tokio::sync::oneshot::channel::<()>();
        let turn_a = scope(a.clone(), async move {
            record(&resp(Some("opus"), "claude-opus-4", 1_000, 100)); // a1
            b_go.send(()).unwrap(); // B starts while A is mid-flight
            a_wait.await.unwrap();
            record(&resp(Some("opus"), "claude-opus-4", 2_000, 200)); // a2
        });
        let turn_b = scope(b.clone(), async move {
            b_wait.await.unwrap();
            record(&resp(Some("local"), "qwen3", 500, 50)); // b1
            a_go.send(()).unwrap();
        });
        tokio::join!(turn_a, turn_b);
        record(&resp(Some("opus"), "claude-opus-4", 9_999, 999)); // no turn

        assert_eq!(a.totals(), (3_000, 300), "A: a1 + a2, nothing of B's");
        assert_eq!(a.to_json().as_array().unwrap().len(), 1);
        assert_eq!(b.totals(), (500, 50), "B: b1 only");
        assert_eq!(b.to_json()[0]["registry_key"], "local");
    }
}
