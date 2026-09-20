/// #1287, same guarantee as `ollama.rs`'s: `AnthropicClient::new` is the
/// constructor `from_env` delegates to, and `preview.rs` calls `from_env`.
/// The 180 s total timeout that used to live here bounded streamed bodies
/// too; neither it nor a read timeout may come back.
#[test]
fn the_self_built_client_has_no_response_clock() {
    let printed = format!(
        "{:?}",
        super::AnthropicClient::new("http://x".into(), "k".into(), "m".into()).http
    );
    assert!(!printed.contains("read_timeout"), "{printed}");
    assert!(!printed.contains("timeout: Some"), "{printed}");
}

use super::*;
use crate::llm::{RichMessage, ToolCallResult, ToolResultEntry};
use serde_json::json;

fn make_client() -> AnthropicClient {
    AnthropicClient::new(
        "http://localhost".into(),
        "test-key".into(),
        "claude-3-5-sonnet-20241022".into(),
    )
}

#[test]
fn effort_is_narrowed_against_the_client_model() {
    // The client's own model decides what may be sent. The fixture client
    // above runs a model with no effort parameter, so a request asking for
    // one must produce no `output_config` at all rather than a 400.
    assert_eq!(
        supported_effort(&make_client().model, mur_common::llm::Effort::Low),
        None
    );
    // A current model takes the level as-is.
    let c = AnthropicClient::new(
        "http://localhost".into(),
        "k".into(),
        "claude-opus-5".into(),
    );
    assert_eq!(
        supported_effort(&c.model, mur_common::llm::Effort::Xhigh),
        Some(mur_common::llm::Effort::Xhigh)
    );
}

#[test]
fn rich_messages_to_anthropic_text_only() {
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
    let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(sys, Some("Be helpful".to_string()));
    assert_eq!(convo.len(), 1);
    assert_eq!(convo[0]["role"], "user");
    assert_eq!(convo[0]["content"], "hi");
}

#[test]
fn image_text_becomes_image_block_then_caption() {
    let msgs = vec![RichMessage::ImageText {
        role: "user".into(),
        media_type: "image/png".into(),
        data: "QkFTRTY0".into(),
        text: "what is this?".into(),
    }];
    let (_, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(convo.len(), 1);
    let content = convo[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["type"], "base64");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert_eq!(content[0]["source"]["data"], "QkFTRTY0");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "what is this?");
}

#[test]
fn image_text_without_caption_is_image_only() {
    let msgs = vec![RichMessage::ImageText {
        role: "user".into(),
        media_type: "image/png".into(),
        data: "QQ==".into(),
        text: String::new(),
    }];
    let (_, convo, _) = rich_messages_to_anthropic(&msgs);
    let content = convo[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "no empty text block");
    assert_eq!(content[0]["type"], "image");
}

#[test]
fn rich_messages_tool_use_and_results() {
    let msgs = vec![
        RichMessage::Text {
            role: "user".into(),
            content: "run".into(),
        },
        RichMessage::ToolUse {
            text: Some("Running bash".into()),
            calls: vec![ToolCallResult {
                call_id: "id1".into(),
                tool_name: "bash".into(),
                input: json!({"command": "echo hi"}),
            }],
        },
        RichMessage::ToolResults {
            results: vec![ToolResultEntry {
                call_id: "id1".into(),
                content: "hi\n".into(),
                is_error: false,
                status: crate::tools::ToolStatus::Ok,
                images: Vec::new(),
            }],
        },
    ];
    let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
    assert!(sys.is_none());
    assert_eq!(convo.len(), 3);
    // assistant message with tool_use
    let asst = &convo[1];
    assert_eq!(asst["role"], "assistant");
    let content = asst["content"].as_array().unwrap();
    let has_tool_use = content.iter().any(|b| b["type"] == "tool_use");
    assert!(has_tool_use, "expected tool_use block");
    // user message with tool_result
    let result_msg = &convo[2];
    assert_eq!(result_msg["role"], "user");
    let result_content = result_msg["content"].as_array().unwrap();
    assert_eq!(result_content[0]["type"], "tool_result");
    assert_eq!(result_content[0]["tool_use_id"], "id1");
}

/// A ledger is one user text block, and it coalesces with the user
/// message that follows it — Anthropic 400s on consecutive user turns.
#[test]
fn turn_ledger_renders_as_user_text_coalesced_with_the_next_message() {
    let memory = crate::turn_ledger::TurnMemory::empty(0);
    let msgs = vec![
        RichMessage::Text {
            role: "user".into(),
            content: "do it".into(),
        },
        RichMessage::Text {
            role: "agent".into(),
            content: "done".into(),
        },
        RichMessage::TurnLedger {
            turn: 3,
            memory: memory.clone(),
        },
        RichMessage::Text {
            role: "user".into(),
            content: "really?".into(),
        },
    ];
    let (_, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(convo.len(), 3, "{convo:?}");
    assert_eq!(convo[2]["role"], "user");
    let blocks = convo[2]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    let rendered = crate::turn_ledger::render_memory(3, &memory);
    assert_eq!(blocks[0]["text"], rendered);
    assert!(rendered.starts_with(crate::turn_ledger::MEMORY_OPEN));
    assert_eq!(blocks[1]["text"], "really?");
}

/// The whole point of the feature: an image on a tool result must reach
/// the wire as a real `image` block inside `tool_result.content`, because
/// that is the only shape the model can actually see.
#[test]
fn tool_result_image_becomes_a_wire_image_block() {
    let msgs = vec![RichMessage::ToolResults {
        results: vec![ToolResultEntry {
            call_id: "id1".into(),
            content: "[image /tmp/car.jpg — image/jpeg, 9 bytes]".into(),
            is_error: false,
            status: Default::default(),
            images: vec![crate::tools::ToolImage {
                media_type: "image/jpeg".into(),
                data: "QUJD".into(),
            }],
        }],
    }];
    let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
    let content = convo[0]["content"][0]["content"]
        .as_array()
        .expect("with an image, tool_result.content must be a block array, not a bare string");
    assert_eq!(content[0]["type"], "text", "text block leads");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
    assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
    assert_eq!(content[1]["source"]["data"], "QUJD");
}

/// Negative control for the test above, and a compatibility guard: with no
/// image the request must be byte-identical to what this adapter always
/// sent — a bare string, not a one-element block array. A silent change
/// here would invalidate every cached prefix in the wild.
#[test]
fn tool_result_without_images_stays_a_bare_string() {
    let msgs = vec![RichMessage::ToolResults {
        results: vec![ToolResultEntry {
            call_id: "id1".into(),
            content: "15 degrees".into(),
            is_error: false,
            status: Default::default(),
            images: vec![],
        }],
    }];
    let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(
        convo[0]["content"][0]["content"], "15 degrees",
        "no image must mean no shape change"
    );
}

#[test]
fn serializes_system_to_top_level() {
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
    let (sys, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(sys, Some("Be helpful".to_string()));
    assert_eq!(convo[0]["role"], "user");
}

#[test]
fn parse_response_body_text_only() {
    let body = json!({
        "content": [{"type": "text", "text": "Done."}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 10, "output_tokens": 5}
    });
    let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
    assert_eq!(text, "Done.");
    assert!(tool_calls.is_empty());
    assert_eq!(stop_reason, crate::llm::StopReason::EndTurn);
}

#[test]
fn parse_response_body_tool_use() {
    let body = json!({
        "content": [
            {"type": "text", "text": "I'll run that."},
            {"type": "tool_use", "id": "call_1", "name": "bash", "input": {"command": "echo hi"}}
        ],
        "stop_reason": "tool_use"
    });
    let (text, tool_calls, stop_reason) = parse_response_body(&body).unwrap();
    assert_eq!(text, "I'll run that.");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].call_id, "call_1");
    assert_eq!(tool_calls[0].tool_name, "bash");
    assert_eq!(stop_reason, crate::llm::StopReason::ToolUse);
}

#[test]
fn rich_to_anthropic_coalesces_consecutive_user_messages() {
    use crate::llm::ToolResultEntry;
    // tool_results (user) immediately followed by an injected user steer.
    let msgs = vec![
        RichMessage::ToolResults {
            results: vec![ToolResultEntry {
                call_id: "c1".into(),
                content: "ok".into(),
                is_error: false,
                status: crate::tools::ToolStatus::Ok,
                images: Vec::new(),
            }],
        },
        RichMessage::Text {
            role: "user".into(),
            content: "(steering) use ripgrep".into(),
        },
    ];
    let (_sys, convo, _) = rich_messages_to_anthropic(&msgs);
    // Must be ONE user message, not two (Anthropic forbids consecutive same-role).
    assert_eq!(
        convo.len(),
        1,
        "consecutive user messages must coalesce: {convo:?}"
    );
    assert_eq!(convo[0]["role"], "user");
    let content = convo[0]["content"].as_array().expect("content array");
    // tool_result block + the steering text block
    assert!(content.iter().any(|b| b["type"] == "tool_result"));
    assert!(
        content
            .iter()
            .any(|b| b["type"] == "text" && b["text"].as_str() == Some("(steering) use ripgrep"))
    );
}

#[test]
fn rich_to_anthropic_keeps_alternating_roles_separate() {
    let msgs = vec![
        RichMessage::Text {
            role: "user".into(),
            content: "hi".into(),
        },
        RichMessage::Text {
            role: "agent".into(),
            content: "hello".into(),
        },
        RichMessage::Text {
            role: "user".into(),
            content: "bye".into(),
        },
    ];
    let (_s, convo, _) = rich_messages_to_anthropic(&msgs);
    assert_eq!(convo.len(), 3);
    assert_eq!(convo[1]["role"], "assistant");
}

#[test]
fn apply_sse_event_streams_text_and_reasoning() {
    let mut acc = StreamAccum::default();
    let d = apply_sse_event(
        &mut acc,
        &json!({
            "type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}
        }),
    );
    assert_eq!(
        d.as_ref().map(|x| (x.text.as_str(), x.thinking)),
        Some(("hello", false))
    );
    assert_eq!(acc.text, "hello");

    let d = apply_sse_event(
        &mut acc,
        &json!({
            "type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hmm"}
        }),
    );
    assert_eq!(
        d.as_ref().map(|x| (x.text.as_str(), x.thinking)),
        Some(("hmm", true))
    );
    // reasoning streamed but NOT accumulated into answer text
    assert_eq!(acc.text, "hello");
}

#[test]
fn apply_sse_event_reconstructs_tool_use_and_stop_reason() {
    let mut acc = StreamAccum::default();
    apply_sse_event(
        &mut acc,
        &json!({
            "type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"call_1","name":"bash","input":{}}
        }),
    );
    apply_sse_event(
        &mut acc,
        &json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"input_json_delta","partial_json":"{\"command\":"}
        }),
    );
    apply_sse_event(
        &mut acc,
        &json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"input_json_delta","partial_json":"\"echo hi\"}"}
        }),
    );
    apply_sse_event(&mut acc, &json!({"type":"content_block_stop","index":0}));
    apply_sse_event(
        &mut acc,
        &json!({
            "type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}
        }),
    );

    assert_eq!(acc.tool_calls.len(), 1);
    assert_eq!(acc.tool_calls[0].call_id, "call_1");
    assert_eq!(acc.tool_calls[0].tool_name, "bash");
    assert_eq!(acc.tool_calls[0].input, json!({"command":"echo hi"}));
    assert_eq!(acc.stop_reason, crate::llm::StopReason::ToolUse);
    assert_eq!(acc.output_tokens, 7);
}

#[test]
fn apply_sse_event_no_arg_tool_defaults_to_empty_object() {
    let mut acc = StreamAccum::default();
    apply_sse_event(
        &mut acc,
        &json!({
            "type":"content_block_start",
            "content_block":{"type":"tool_use","id":"c","name":"now","input":{}}
        }),
    );
    // No input_json_delta events — tool has no args
    apply_sse_event(&mut acc, &json!({"type":"content_block_stop","index":0}));

    assert_eq!(acc.tool_calls.len(), 1);
    assert_eq!(acc.tool_calls[0].input, json!({}));
}

#[test]
fn apply_sse_event_two_sequential_tool_blocks() {
    let mut acc = StreamAccum::default();
    // block 0: tool A
    apply_sse_event(
        &mut acc,
        &serde_json::json!({
            "type":"content_block_start","index":0,
            "content_block":{"type":"tool_use","id":"a","name":"read","input":{}}
        }),
    );
    apply_sse_event(
        &mut acc,
        &serde_json::json!({
            "type":"content_block_delta","index":0,
            "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"x\"}"}
        }),
    );
    apply_sse_event(
        &mut acc,
        &serde_json::json!({"type":"content_block_stop","index":0}),
    );
    // block 1: tool B
    apply_sse_event(
        &mut acc,
        &serde_json::json!({
            "type":"content_block_start","index":1,
            "content_block":{"type":"tool_use","id":"b","name":"bash","input":{}}
        }),
    );
    apply_sse_event(
        &mut acc,
        &serde_json::json!({
            "type":"content_block_delta","index":1,
            "delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}
        }),
    );
    apply_sse_event(
        &mut acc,
        &serde_json::json!({"type":"content_block_stop","index":1}),
    );

    assert_eq!(acc.tool_calls.len(), 2);
    assert_eq!(acc.tool_calls[0].call_id, "a");
    assert_eq!(acc.tool_calls[0].tool_name, "read");
    assert_eq!(acc.tool_calls[0].input, serde_json::json!({"path":"x"}));
    assert_eq!(acc.tool_calls[1].call_id, "b");
    assert_eq!(acc.tool_calls[1].input, serde_json::json!({"command":"ls"}));
}

#[test]
fn finish_stream_errors_on_truly_empty_response() {
    let acc = StreamAccum::default();
    let err = finish_stream(acc, "claude-x".into(), false).unwrap_err();
    assert!(matches!(err, LlmError::InvalidResponse(_)));
}

#[test]
fn finish_stream_allows_empty_text_when_truncated_mid_thinking() {
    // Regression: a turn that spends its whole max_tokens budget inside a
    // thinking block, before any text or tool_use block starts, must not
    // be treated as a malformed response — it should come back as a
    // (textless) MaxTokens result so task_runner can retry with guidance.
    let acc = StreamAccum {
        stop_reason: StopReason::MaxTokens,
        ..StreamAccum::default()
    };
    let resp = finish_stream(acc, "claude-x".into(), false).expect("should not error");
    assert_eq!(resp.text, "");
    assert!(resp.tool_calls.is_empty());
    assert_eq!(resp.stop_reason, StopReason::MaxTokens);
}

#[test]
fn finish_stream_keeps_erroring_on_empty_text_for_normal_stop() {
    let acc = StreamAccum {
        stop_reason: StopReason::EndTurn,
        ..StreamAccum::default()
    };
    assert!(finish_stream(acc, "claude-x".into(), false).is_err());
}

fn ok_message() -> serde_json::Value {
    json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "hi"}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 1, "output_tokens": 1}
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

/// The authless constructor sends no `x-api-key` and no `Authorization`
/// at all. The gateway picks its mode by header *presence*: an absent
/// header means "attach the keychain token", an empty one means
/// "pass through untouched" — and a 401 from Anthropic.
#[tokio::test]
async fn authless_client_sends_no_credential_header() {
    let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
    let server = httpmock::MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/messages")
                .matches(|req| {
                    !req.headers.as_ref().is_some_and(|h| {
                        h.iter().any(|(k, _)| {
                            k.eq_ignore_ascii_case("x-api-key")
                                || k.eq_ignore_ascii_case("authorization")
                        })
                    })
                });
            then.status(200).json_body(ok_message());
        })
        .await;
    let client = AnthropicClient::authless_with_http(
        server.base_url(),
        "claude-opus-5".into(),
        crate::sandbox::reqwest_guard::GuardedHttpClient::unrestricted(
            crate::llm::llm_client_builder(),
        )
        .unwrap(),
    );
    let resp = client.generate(hello()).await.unwrap();
    assert_eq!(resp.text, "hi");
    m.assert_async().await;
}

/// Existing constructors are unchanged: `new` still sends the key.
#[tokio::test]
async fn keyed_client_still_sends_x_api_key() {
    let _serial = crate::llm::MOCK_SERVER_LOCK.lock().await;
    let server = httpmock::MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/v1/messages")
                .header("x-api-key", "test-key");
            then.status(200).json_body(ok_message());
        })
        .await;
    let client = AnthropicClient::new(server.base_url(), "test-key".into(), "claude-opus-5".into());
    client.generate(hello()).await.unwrap();
    m.assert_async().await;
}

/// The gap this closes: a subscription token pointed at the loopback
/// bridge used to be classified as fine, because the only check was
/// "is the base URL api.anthropic.com". The bridge drops the key and
/// attaches its own, so nothing about that config does what it looks
/// like it does.
#[test]
fn an_oauth_key_at_a_loopback_bridge_is_flagged_as_ignored() {
    for base in [
        "http://127.0.0.1:8088",
        "http://localhost:8088/v1",
        "http://[::1]:8088",
    ] {
        assert_eq!(
            classify_oauth_key("sk-ant-oat01-abc", base),
            Some(OauthKeyMisuse::IgnoredByBridge),
            "{base}"
        );
    }
}

#[test]
fn an_oauth_key_at_anthropic_is_still_flagged_as_rejected() {
    assert_eq!(
        classify_oauth_key("sk-ant-oat01-abc", "https://api.anthropic.com"),
        Some(OauthKeyMisuse::RejectedUpstream)
    );
}

/// A real API key is fine everywhere, and a remote bridge we know nothing
/// about must not be second-guessed — it may well forward the token.
#[test]
fn a_real_key_and_an_unknown_remote_are_left_alone() {
    assert_eq!(
        classify_oauth_key("sk-ant-api03-abc", "https://api.anthropic.com"),
        None
    );
    assert_eq!(
        classify_oauth_key("sk-ant-api03-abc", "http://127.0.0.1:8088"),
        None
    );
    assert_eq!(
        classify_oauth_key("sk-ant-oat01-abc", "https://bridge.example.com"),
        None
    );
}

#[test]
fn anthropic_error_mapping_uses_anchored_prompt_overflow_shape() {
    use crate::llm::{Disposition, classify};
    let exact = json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long"}}).to_string();
    let error = super::map_anthropic_error(400, &exact);
    assert!(matches!(error, LlmError::ContextExceeded(_)), "{error:?}");
    assert_eq!(classify(&error), Disposition::AdvanceNow);

    for message in [
        "your prompt is too long",
        "prompt is too long for your spend limit",
        "prompt is too long ",
    ] {
        let body =
            json!({"type":"error","error":{"type":"invalid_request_error","message":message}})
                .to_string();
        let error = super::map_anthropic_error(400, &body);
        assert!(
            matches!(error, LlmError::Rejected(400, _)),
            "{message}: {error:?}"
        );
        assert_eq!(classify(&error), Disposition::Stop);
    }
}

#[test]
fn anthropic_permission_safety_and_size_fail_closed() {
    use crate::llm::{Disposition, classify};
    let fixtures = [
        (
            json!({"type":"error","error":{"type":"permission_error","message":"organization denied access"}}),
            "permission",
        ),
        (
            json!({"type":"error","error":{"type":"invalid_request_error","code":"safety_policy_violation","message":"request refused"}}),
            "safety",
        ),
        (
            json!({"type":"error","error":{"type":"invalid_request_error","message":"organization spend limit exceeded"}}),
            "spend",
        ),
    ];
    for (value, kind) in fixtures {
        let error = super::map_anthropic_error(400, &value.to_string());
        assert_eq!(classify(&error), Disposition::Stop, "{kind}: {error:?}");
        match kind {
            "permission" => assert!(matches!(error, LlmError::PermissionDenied(400, _))),
            "safety" => assert!(matches!(error, LlmError::SafetyPolicyRejected(_))),
            "spend" => assert!(matches!(error, LlmError::Rejected(400, _))),
            _ => unreachable!(),
        }
    }
    let too_large = super::map_anthropic_error(
        413,
        r#"{"type":"error","error":{"type":"request_too_large","message":"request exceeds 32 MB"}}"#,
    );
    assert!(
        matches!(too_large, LlmError::Rejected(413, _)),
        "{too_large:?}"
    );
    assert_eq!(classify(&too_large), Disposition::Stop);
}
