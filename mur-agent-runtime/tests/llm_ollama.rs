use httpmock::prelude::*;
use mur_agent_runtime::llm::ollama::OllamaClient;
use mur_agent_runtime::llm::{LlmClient, LlmRequest, RichMessage};
use serde_json::json;

#[tokio::test]
async fn ollama_generate_returns_text_and_usage() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/api/chat");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "model": "llama3.2",
                    "message": {"role": "assistant", "content": "Hello back"},
                    "prompt_eval_count": 10,
                    "eval_count": 5,
                    "done": true
                }));
        })
        .await;
    let client = OllamaClient::new(server.base_url(), "llama3.2".into());
    let resp = client
        .generate(LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "Hi".into(),
            }],
            temperature: Some(0.2),
            max_tokens: Some(100),
            tools: vec![],
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(resp.text, "Hello back");
    assert_eq!(resp.input_tokens, 10);
    assert_eq!(resp.output_tokens, 5);
}
