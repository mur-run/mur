//! The model half of pre-dispatch triage: the `ask` closure that
//! [`super::triage::triage`] takes, backed by the local OpenAI-compatible
//! endpoint MUR already runs.
//!
//! This module owns transport only. It does NOT decide anything: it returns
//! the model's raw reply, and `triage::parse_verdict` / `triage::decide` are
//! what bind it. Keeping the split means a compromised or confused model can
//! at worst produce a verdict that `decide` overrides, never a decision.
//!
//! Every failure is an `Err(String)` carrying why, because the caller's
//! contract is to degrade to `Proceed` with that string recorded — a triage
//! model that can panic or hang is a new way to take the fleet down.

use std::time::Duration;

use super::triage::TRIAGE_SYSTEM;

/// A triage model reachable over an OpenAI-compatible `/chat/completions`.
#[derive(Debug, Clone)]
pub struct LocalTriageModel {
    /// Base URL including any `/v1` suffix, e.g. `http://127.0.0.1:8721/v1`.
    base_url: String,
    model_id: String,
}

impl LocalTriageModel {
    /// Point at an explicit endpoint. Used by tests and by any caller that
    /// wants triage to run somewhere other than the bundled model.
    pub fn new(base_url: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model_id: model_id.into(),
        }
    }

    /// Resolve the local endpoint MUR Hub publishes. `Err` when the Hub is
    /// not running — the caller degrades, it does not block dispatch.
    pub fn from_home(home: &std::path::Path) -> Result<Self, String> {
        let base = mur_common::local_llm::read_base_url(home).ok_or_else(|| {
            "local model endpoint not available (is MUR Hub running?)".to_string()
        })?;
        Ok(Self::new(
            base,
            mur_common::config::DEFAULT_BUNDLED_MODEL_ID,
        ))
    }

    /// The request body. Split out so a test can assert its shape without a
    /// server, and so the Qwen3 thinking trap is visible in one place.
    pub fn request_body(&self, prompt: &str) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model_id,
            "temperature": 0,
            "messages": [
                { "role": "system", "content": TRIAGE_SYSTEM },
                { "role": "user", "content": prompt }
            ],
            "max_tokens": 512
        });
        // The bundled Qwen3 spends its whole budget on chain-of-thought and
        // returns empty `message.content` unless this is off — the same trap
        // `cmd/media/analyze.rs:325` documents.
        if self.model_id.to_lowercase().contains("qwen3")
            && let Some(obj) = body.as_object_mut()
        {
            obj.insert(
                "chat_template_kwargs".into(),
                serde_json::json!({ "enable_thinking": false }),
            );
        }
        body
    }

    /// Ask the model. Returns its raw text reply; parsing is not this
    /// module's job.
    ///
    /// `timeout` is enforced on the whole request, not just connect: the
    /// point of triage is to cost less than the run it might prevent, so a
    /// model that stalls must lose, not hold the dispatcher.
    pub async fn ask(&self, prompt: String, timeout: Duration) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| format!("triage client: {e}"))?;

        let resp = client
            .post(&url)
            .json(&self.request_body(&prompt))
            .send()
            .await
            .map_err(|e| format!("triage request to {url} failed: {e}"))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("triage response body ({status}): {e}"))?;

        if !status.is_success() {
            // The status goes in the message because the caller records this
            // string verbatim as the reason triage degraded.
            let tail: String = body.chars().take(200).collect();
            return Err(format!("triage endpoint returned {status}: {tail}"));
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| format!("triage response was not JSON: {e}"))?;
        extract_content(&json)
    }
}

/// Pull `choices[0].message.content` out of an OpenAI-compatible response.
/// An empty or absent content is an error, not an empty verdict: silently
/// returning `""` would surface as "unparseable verdict" and hide the fact
/// that the model never said anything.
pub fn extract_content(resp: &serde_json::Value) -> Result<String, String> {
    let choice = resp
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| {
            // Surface the endpoint's own error when it sent one; "no choices"
            // alone has sent people hunting in the wrong crate before.
            let why = resp
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("response carried no choices");
            format!("triage response had no choices: {why}")
        })?;

    let content = choice
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| "triage response had no message.content".to_string())?;

    if content.trim().is_empty() {
        return Err(
            "triage model returned empty content (thinking mode may have eaten max_tokens)".into(),
        );
    }
    Ok(content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::triage::{Basis, Decision, triage};
    use mur_common::limits::{Resolved, ResolvedLimits, Source, Stuck};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn limits(deadline_secs: Option<u64>, cost: Option<f64>) -> ResolvedLimits {
        ResolvedLimits {
            deadline: Resolved {
                value: deadline_secs.map(Duration::from_secs),
                source: Source::BuiltIn,
            },
            stuck: Resolved {
                value: Stuck::After(Duration::from_secs(600)),
                source: Source::BuiltIn,
            },
            cost_usd: Resolved {
                value: cost,
                source: Source::BuiltIn,
            },
        }
    }

    const GOOD_VERDICT: &str = r#"{"complexity":"XL","ambiguity":4,"dependency_risk":3,
        "recommended_action":"split","proposed_splits":["extract the gate","wire the ceiling"],
        "confidence":0.9,"reasons":["touches two crates"]}"#;

    fn completion(content: &str) -> String {
        serde_json::json!({
            "choices": [ { "message": { "role": "assistant", "content": content } } ]
        })
        .to_string()
    }

    // ---- request shape ----

    #[test]
    fn the_request_carries_the_triage_system_prompt_and_the_task() {
        let m = LocalTriageModel::new("http://x/v1", "some-model");
        let b = m.request_body("Task: refactor the executor");
        let msgs = b["messages"].as_array().expect("messages array");
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(
            msgs[0]["content"].as_str().unwrap(),
            TRIAGE_SYSTEM,
            "the system prompt must be the one that forbids turn estimates"
        );
        assert_eq!(msgs[1]["role"], "user");
        assert!(
            msgs[1]["content"]
                .as_str()
                .unwrap()
                .contains("refactor the executor")
        );
    }

    #[test]
    fn triage_is_deterministic() {
        let m = LocalTriageModel::new("http://x/v1", "some-model");
        assert_eq!(
            m.request_body("t")["temperature"],
            0,
            "a verdict that changes between identical runs is not a verdict"
        );
    }

    #[test]
    fn the_bundled_qwen3_has_thinking_turned_off() {
        // Without this the model burns max_tokens on reasoning and returns
        // empty content — the verdict would always be "unparseable".
        let m = LocalTriageModel::new("http://x/v1", "Qwen3.5-2B-MLX-4bit");
        assert_eq!(
            m.request_body("t")["chat_template_kwargs"]["enable_thinking"],
            false
        );
    }

    #[test]
    fn a_non_qwen_model_gets_no_vendor_specific_kwarg() {
        let m = LocalTriageModel::new("http://x/v1", "gpt-4o-mini");
        assert!(
            m.request_body("t").get("chat_template_kwargs").is_none(),
            "an unknown field can be rejected by other endpoints"
        );
    }

    // ---- response handling ----

    #[test]
    fn content_is_pulled_out_of_a_normal_completion() {
        let resp: serde_json::Value = serde_json::from_str(&completion("hello")).unwrap();
        assert_eq!(extract_content(&resp).unwrap(), "hello");
    }

    #[test]
    fn an_empty_content_is_an_error_not_an_empty_verdict() {
        let resp: serde_json::Value = serde_json::from_str(&completion("   ")).unwrap();
        let e = extract_content(&resp).expect_err("blank content must not read as a reply");
        assert!(e.to_lowercase().contains("empty"), "{e}");
    }

    #[test]
    fn a_response_with_no_choices_says_so() {
        let resp = serde_json::json!({ "error": { "message": "model not loaded" } });
        let e = extract_content(&resp).expect_err("no choices must be an error");
        assert!(!e.is_empty(), "the failure must carry why");
    }

    // ---- transport ----

    #[tokio::test]
    async fn a_live_model_returns_its_raw_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(completion(GOOD_VERDICT)))
            .mount(&server)
            .await;

        let m = LocalTriageModel::new(format!("{}/v1", server.uri()), "some-model");
        let raw = m
            .ask("Task: refactor".into(), Duration::from_secs(5))
            .await
            .expect("a 200 with content should succeed");
        assert!(raw.contains("\"recommended_action\":\"split\""), "{raw}");
    }

    #[tokio::test]
    async fn a_500_is_an_error_carrying_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let m = LocalTriageModel::new(format!("{}/v1", server.uri()), "some-model");
        let e = m
            .ask("t".into(), Duration::from_secs(5))
            .await
            .expect_err("a 500 must not read as a verdict");
        assert!(e.contains("500"), "the failure must name the status: {e}");
    }

    #[tokio::test]
    async fn a_trailing_slash_on_the_base_url_does_not_produce_a_double_slash() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(completion(GOOD_VERDICT)))
            .mount(&server)
            .await;

        let m = LocalTriageModel::new(format!("{}/v1/", server.uri()), "some-model");
        assert!(
            m.ask("t".into(), Duration::from_secs(5)).await.is_ok(),
            "the endpoint is read from a file; a trailing slash is not a user error"
        );
    }

    #[tokio::test]
    async fn a_dead_endpoint_is_an_error_not_a_panic() {
        // Port 1 is reserved and nothing listens there.
        let m = LocalTriageModel::new("http://127.0.0.1:1/v1", "some-model");
        assert!(m.ask("t".into(), Duration::from_secs(2)).await.is_err());
    }

    // ---- wired end to end ----

    #[tokio::test]
    async fn a_real_endpoint_drives_a_real_decision() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(completion(GOOD_VERDICT)))
            .mount(&server)
            .await;

        let m = LocalTriageModel::new(format!("{}/v1", server.uri()), "some-model");
        let l = limits(Some(1800), Some(5.0));
        let outcome = triage(
            "refactor the tier ceiling across mur-core and mur-agent-runtime",
            &l,
            Duration::from_secs(5),
            |p| async move { m.ask(p, Duration::from_secs(5)).await },
        )
        .await;

        assert_eq!(outcome.decision, Decision::Split);
        assert!(
            matches!(outcome.basis, Basis::Verdict(_)),
            "{:?}",
            outcome.basis
        );
        assert_eq!(
            outcome.limits, l,
            "triage must echo the budget, never widen it"
        );
    }

    #[tokio::test]
    async fn a_dead_model_over_real_transport_still_never_blocks_dispatch() {
        let m = LocalTriageModel::new("http://127.0.0.1:1/v1", "some-model");
        let l = limits(Some(1800), Some(5.0));
        let outcome = triage(
            "refactor the executor",
            &l,
            Duration::from_secs(3),
            |p| async move { m.ask(p, Duration::from_secs(2)).await },
        )
        .await;

        assert_eq!(
            outcome.decision,
            Decision::Proceed,
            "killing the triage model must not be how you stop the fleet"
        );
        assert!(
            matches!(outcome.basis, Basis::Degraded(_)),
            "{:?}",
            outcome.basis
        );
    }

    #[test]
    fn from_home_without_a_running_hub_is_an_error_not_a_default_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let e = LocalTriageModel::from_home(tmp.path())
            .expect_err("a missing endpoint must not silently become localhost");
        assert!(e.contains("Hub"), "{e}");
    }
}
