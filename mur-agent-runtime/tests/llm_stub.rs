//! StubLlm provider tests (M3.3).

use mur_agent_runtime::llm::stub::StubLlm;
use mur_agent_runtime::llm::{LlmClient, LlmError, LlmRequest, RichMessage};

fn req(text: &str) -> LlmRequest {
    LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: text.into(),
        }],
        temperature: None,
        max_tokens: None,
        tools: vec![],
        ..Default::default()
    }
}

#[tokio::test]
async fn stub_returns_canned_response_by_substring_match() {
    let stub = StubLlm::with_default_scenarios();
    let resp = stub
        .generate(req("Please write a morning_greeting in zh-TW."))
        .await
        .unwrap();
    assert!(resp.text.contains("早安"), "got: {}", resp.text);
}

#[tokio::test]
async fn stub_simulates_rate_limit_429() {
    let stub = StubLlm::with_default_scenarios();
    let err = stub
        .generate(req("FAULT_429 morning_greeting"))
        .await
        .unwrap_err();
    assert!(matches!(err, LlmError::RateLimit));
}

#[tokio::test]
async fn stub_unmatched_returns_stable_echo() {
    let stub = StubLlm::with_default_scenarios();
    let resp = stub.generate(req("totally unknown content")).await.unwrap();
    assert_eq!(resp.text, "[stub: no scenario matched]");
}

#[tokio::test]
async fn stub_translate_scenario() {
    let stub = StubLlm::with_default_scenarios();
    let resp = stub.generate(req("translate this to zh-TW")).await.unwrap();
    assert_eq!(resp.text, "<<TRANSLATED>>");
}
