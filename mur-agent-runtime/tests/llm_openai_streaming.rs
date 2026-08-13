//! Streaming tool-call assembly for the OpenAI-compatible client.
//!
//! Regression cover for #938: `generate_stream` used to read only
//! `delta.content` and return a hardcoded empty `tool_calls`, so every tool
//! call a model made over this transport was discarded and its narration was
//! returned as the answer. These tests drive the real streaming path against
//! the two chunk shapes seen in production.

use httpmock::prelude::*;
use mur_agent_runtime::llm::openai::OpenAiClient;
use mur_agent_runtime::llm::{LlmClient, LlmRequest, RichMessage, StopReason};

fn user(text: &str) -> LlmRequest {
    LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: text.into(),
        }],
        max_tokens: Some(200),
        ..Default::default()
    }
}

/// Drive `generate_stream` against a canned SSE body and return the response.
async fn stream_of(sse: &str) -> mur_agent_runtime::llm::LlmResponse {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(sse);
        })
        .await;
    let client = OpenAiClient::new(server.base_url(), "k".into(), "test-model".into());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    client
        .generate_stream(user("what branch?"), tx)
        .await
        .expect("streamed tool call must not be an error")
}

/// oMLX (and other single-shot servers) send the whole call in one delta.
#[tokio::test]
async fn whole_tool_call_in_one_delta_is_assembled() {
    let sse = concat!(
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_3dfd3226","type":"function","function":{"name":"bash","arguments":"{\"command\": \"git branch --show-current\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\ndata: [DONE]\n\n",
    );
    let resp = stream_of(sse).await;
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    assert_eq!(resp.tool_calls.len(), 1, "the call must survive the stream");
    let call = &resp.tool_calls[0];
    assert_eq!(call.call_id, "call_3dfd3226");
    assert_eq!(call.tool_name, "bash");
    assert_eq!(call.input["command"], "git branch --show-current");
}

/// DeepSeek opens the call with an empty `arguments`, then streams the JSON in
/// fragments. Concatenating them in order is the whole point of the accumulator.
#[tokio::test]
async fn fragmented_arguments_are_concatenated_in_order() {
    let sse = concat!(
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":null}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_00","type":"function","function":{"name":"bash","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"comm"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\": \"git "}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"status\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\ndata: [DONE]\n\n",
    );
    let resp = stream_of(sse).await;
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].call_id, "call_00");
    assert_eq!(resp.tool_calls[0].tool_name, "bash");
    assert_eq!(
        resp.tool_calls[0].input["command"], "git status",
        "argument fragments must be joined, not overwritten"
    );
}

/// Two calls opened in the same turn are kept apart by their `index`, and a
/// later fragment carrying only `arguments` must not blank the name or id.
#[tokio::test]
async fn parallel_calls_are_kept_separate_by_index() {
    let sse = concat!(
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"bash","arguments":"{\"command\":\"pwd\"}"}},{"index":1,"id":"b","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\":\"/tmp/x\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\ndata: [DONE]\n\n",
    );
    let resp = stream_of(sse).await;
    assert_eq!(resp.tool_calls.len(), 2);
    assert_eq!(resp.tool_calls[0].tool_name, "bash");
    assert_eq!(resp.tool_calls[0].input["command"], "pwd");
    assert_eq!(
        resp.tool_calls[1].tool_name, "read_file",
        "a fragment with no name must not clobber the established one"
    );
    assert_eq!(resp.tool_calls[1].call_id, "b");
    assert_eq!(resp.tool_calls[1].input["path"], "/tmp/x");
}

/// A turn that goes straight to a tool call carries no text. That is a
/// complete response, not the blank reply the empty-stream guard is for.
#[tokio::test]
async fn tool_call_without_text_is_not_an_empty_response() {
    let sse = concat!(
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
        "\n\n",
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "\n\ndata: [DONE]\n\n",
    );
    let resp = stream_of(sse).await;
    assert!(resp.text.is_empty());
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
}

/// Neither text nor calls is still an error — the guard must keep catching the
/// genuinely blank reply it was written for.
#[tokio::test]
async fn stream_with_neither_text_nor_calls_still_errors() {
    let server = MockServer::start_async().await;
    let _mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/chat/completions");
            then.status(200)
                .header("content-type", "text/event-stream")
                .body(concat!(
                    r#"data: {"choices":[{"index":0,"delta":{"content":""}}]}"#,
                    "\n\ndata: [DONE]\n\n",
                ));
        })
        .await;
    let client = OpenAiClient::new(server.base_url(), "k".into(), "test-model".into());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let err = client
        .generate_stream(user("hi"), tx)
        .await
        .expect_err("a reply with no text and no calls must still be rejected");
    assert!(
        format!("{err}").contains("empty streamed response"),
        "got: {err}"
    );
}
