use super::*;
use crate::llm::{RichMessage, ToolCallResult, ToolResultEntry};
use serde_json::json;

#[test]
fn rich_messages_to_openai_text_only() {
    let msgs = vec![
        RichMessage::Text {
            role: "system".into(),
            content: "Be helpful".into(),
        },
        RichMessage::Text {
            role: "user".into(),
            content: "hi".into(),
        },
    ];
    let result = rich_messages_to_openai(&msgs);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0]["role"], "system");
    assert_eq!(result[1]["role"], "user");
}

#[test]
fn rich_messages_image_text_becomes_image_url_block() {
    let msgs = vec![RichMessage::ImageText {
        role: "user".into(),
        media_type: "image/png".into(),
        data: "QkFTRTY0".into(),
        text: "what color?".into(),
    }];
    let out = rich_messages_to_openai(&msgs);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], "user");
    let content = out[0]["content"].as_array().expect("multimodal array");
    assert_eq!(content[0]["type"], "image_url");
    assert_eq!(
        content[0]["image_url"]["url"],
        "data:image/png;base64,QkFTRTY0"
    );
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "what color?");
}

#[test]
fn rich_messages_image_only_omits_empty_text_block() {
    let msgs = vec![RichMessage::ImageText {
        role: "user".into(),
        media_type: "image/jpeg".into(),
        data: "QQ==".into(),
        text: String::new(),
    }];
    let out = rich_messages_to_openai(&msgs);
    let content = out[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "no empty text block");
    assert_eq!(
        content[0]["image_url"]["url"],
        "data:image/jpeg;base64,QQ=="
    );
}

#[test]
fn rich_messages_tool_use_and_results() {
    let msgs = vec![
        RichMessage::Text {
            role: "user".into(),
            content: "run it".into(),
        },
        RichMessage::ToolUse {
            text: Some("Running bash".into()),
            calls: vec![ToolCallResult {
                call_id: "call_abc".into(),
                tool_name: "bash".into(),
                input: json!({"command": "echo hi"}),
            }],
        },
        RichMessage::ToolResults {
            results: vec![ToolResultEntry {
                call_id: "call_abc".into(),
                content: "hi\n".into(),
                is_error: false,
                status: crate::tools::ToolStatus::Ok,
                images: Vec::new(),
            }],
        },
    ];
    let result = rich_messages_to_openai(&msgs);
    assert_eq!(result.len(), 3);
    // assistant message with tool_calls
    let asst = &result[1];
    assert_eq!(asst["role"], "assistant");
    let tc = &asst["tool_calls"][0];
    assert_eq!(tc["id"], "call_abc");
    assert_eq!(tc["function"]["name"], "bash");
    // tool result message
    let tool_msg = &result[2];
    assert_eq!(tool_msg["role"], "tool");
    assert_eq!(tool_msg["tool_call_id"], "call_abc");
}

#[test]
fn parse_response_body_text_only() {
    let body = json!({
        "choices": [{"message": {"content": "Hello", "tool_calls": null}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2}
    });
    let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
    assert_eq!(text, "Hello");
    assert!(tool_calls.is_empty());
    assert_eq!(stop_reason, crate::llm::StopReason::EndTurn);
}

#[test]
fn parse_response_body_tool_calls() {
    let args = json!({"command": "echo hi"}).to_string();
    let body = json!({
        "choices": [{
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "call_abc",
                    "function": {"name": "bash", "arguments": args}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    });
    let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
    assert_eq!(text, "");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].call_id, "call_abc");
    assert_eq!(tool_calls[0].tool_name, "bash");
    assert_eq!(stop_reason, crate::llm::StopReason::ToolUse);
}

fn ok_completion() -> serde_json::Value {
    json!({
        "choices": [{"message": {"role": "assistant", "content": "hi"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    })
}

fn hello() -> LlmRequest {
    LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: "hi".into(),
        }],
        ..Default::default()
    }
}

/// The authless constructor sends neither `Authorization` nor `x-api-key`:
/// the loopback gateway attaches the subscription token itself.
#[tokio::test]
async fn authless_client_sends_no_credential_header() {
    let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
    let server = httpmock::MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/chat/completions")
                .matches(|req| {
                    !req.headers.as_ref().is_some_and(|h| {
                        h.iter().any(|(k, _)| {
                            k.eq_ignore_ascii_case("authorization")
                                || k.eq_ignore_ascii_case("x-api-key")
                        })
                    })
                });
            then.status(200).json_body(ok_completion());
        })
        .await;
    let client = OpenAiClient::authless_with_http(
        server.base_url(),
        "gpt-test".into(),
        crate::sandbox::reqwest_guard::GuardedHttpClient::unrestricted(
            crate::llm::llm_client_builder(),
        )
        .unwrap(),
    );
    let resp = client.generate(hello()).await.unwrap();
    assert_eq!(resp.text, "hi");
    m.assert_async().await;
}

/// Existing constructors are unchanged: `new` still sends the bearer key.
#[tokio::test]
async fn keyed_client_still_sends_bearer() {
    let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
    let server = httpmock::MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/chat/completions")
                .header("authorization", "Bearer test-key");
            then.status(200).json_body(ok_completion());
        })
        .await;
    let client = OpenAiClient::new(server.base_url(), "test-key".into(), "gpt-test".into());
    client.generate(hello()).await.unwrap();
    m.assert_async().await;
}

/// #1287: `OpenAiClient::new` is what `from_env` delegates to, and
/// `mur agent companion preview` calls `from_env`
/// (`mur-core/src/cmd/agent_companion/preview.rs`). The 60 s TOTAL timeout that
/// used to live here bounded streamed bodies as well as think time; neither it
/// nor a read timeout may come back. Those are the only two clocks that end a
/// live response instead of a dead connection.
#[test]
fn the_self_built_client_has_no_response_clock() {
    let printed = format!(
        "{:?}",
        OpenAiClient::new("http://x".into(), "k".into(), "m".into()).http
    );
    assert!(!printed.contains("read_timeout"), "{printed}");
    assert!(!printed.contains("timeout: Some"), "{printed}");
}

#[test]
fn openai_error_codes_map_only_enumerated_candidate_failures() {
    use crate::llm::{Disposition, LlmError, classify};
    let cases = [
        (
            "context_length_exceeded",
            Disposition::AdvanceNow,
            "context",
        ),
        ("model_not_found", Disposition::AdvanceNow, "model"),
        ("model_unavailable", Disposition::AdvanceNow, "model"),
        ("insufficient_quota", Disposition::Stop, "credit"),
        ("content_policy_violation", Disposition::Stop, "safety"),
    ];
    for (code, disposition, kind) in cases {
        let body = json!({"error":{"message":"fixture detail","type":"invalid_request_error","code":code}}).to_string();
        let error = super::map_openai_error(400, &body);
        assert_eq!(classify(&error), disposition, "{code}: {error:?}");
        match kind {
            "context" => assert!(matches!(error, LlmError::ContextExceeded(_))),
            "model" => assert!(matches!(error, LlmError::ModelNotFound(_))),
            "credit" => assert!(matches!(error, LlmError::InsufficientCredit)),
            "safety" => assert!(matches!(error, LlmError::SafetyPolicyRejected(_))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn openai_unknown_client_code_stops() {
    let body = json!({"error":{"message":"nope","type":"invalid_request_error","code":"new_provider_code"}}).to_string();
    let error = super::map_openai_error(422, &body);
    assert!(matches!(error, LlmError::Rejected(422, _)), "{error:?}");
    assert_eq!(crate::llm::classify(&error), crate::llm::Disposition::Stop);
}

#[test]
fn turn_ledger_renders_as_a_user_message() {
    let memory = crate::turn_ledger::TurnMemory::empty(1);
    let msgs = vec![
        RichMessage::Text {
            role: "agent".into(),
            content: "done".into(),
        },
        RichMessage::TurnLedger {
            turn: 9,
            memory: memory.clone(),
        },
    ];
    let out = rich_messages_to_openai(&msgs);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1]["role"], "user");
    assert_eq!(
        out[1]["content"],
        crate::turn_ledger::render_memory(9, &memory)
    );
    assert!(
        out[1]["content"]
            .as_str()
            .unwrap()
            .ends_with(crate::turn_ledger::MEMORY_CLOSE)
    );
}
