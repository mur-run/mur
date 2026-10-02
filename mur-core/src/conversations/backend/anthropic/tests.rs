use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn sampling_temperature_dropped_on_models_that_reject_it() {
    // These 400 if `temperature` is present. The guard used to name only
    // Opus 4.7, so making a newer model the default silently armed a 400
    // on every request carrying a temperature.
    for m in [
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-sonnet-5",
        "claude-fable-5",
    ] {
        assert_eq!(sampling_temperature(m, Some(0.5)), None, "{m}");
    }
    // Prefix match, so a dated or suffixed variant is covered too.
    assert_eq!(
        sampling_temperature("claude-opus-5-preview", Some(0.5)),
        None
    );
    // Models that still accept it keep the caller's value.
    assert_eq!(
        sampling_temperature("claude-haiku-4-5", Some(0.5)),
        Some(0.5)
    );
    assert_eq!(
        sampling_temperature("claude-opus-4-6", Some(0.5)),
        Some(0.5)
    );
    // Absent stays absent.
    assert_eq!(sampling_temperature("claude-haiku-4-5", None), None);
}

fn req<'a>(model: &'a str, user: &'a str) -> ChatRequest<'a> {
    ChatRequest {
        model,
        system: None,
        user,
        max_tokens: 16,
        temperature: Some(0.5),
        stop: vec![],
        cache_system: false,
        cache_user_prefix: None,
    }
}

#[tokio::test]
async fn provider_name_is_anthropic() {
    let b = AnthropicBackend::new("http://127.0.0.1:1", "k", Duration::from_millis(100));
    assert_eq!(b.provider_name(), "anthropic");
}

#[test]
fn supports_caching_is_true_for_anthropic() {
    let b = AnthropicBackend::new("http://unused", "k", Duration::from_millis(100));
    assert!(b.supports_caching());
}

#[tokio::test]
async fn cache_system_true_emits_system_block_with_cache_control_ephemeral() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let mut r = req("claude-haiku-4-5", "hi");
    r.system = Some("you are a tester");
    r.cache_system = true;
    let _ = b.generate(r).await.unwrap();

    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    // system MUST be a JSON array of blocks (not a plain string) when caching
    let system = body.get("system").expect("system field present");
    assert!(
        system.is_array(),
        "expected system to be a block array, got {system:?}"
    );
    let arr = system.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("type").and_then(|v| v.as_str()), Some("text"));
    assert_eq!(
        arr[0].get("text").and_then(|v| v.as_str()),
        Some("you are a tester")
    );
    assert_eq!(
        arr[0]
            .get("cache_control")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str()),
        Some("ephemeral"),
        "expected cache_control: {{type: ephemeral}} on the system block"
    );
}

#[tokio::test]
async fn cache_user_prefix_emits_two_block_user_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let mut r = req("claude-haiku-4-5", "PREFIX_BLOCKsuffix");
    r.cache_user_prefix = Some("PREFIX_BLOCK".len());
    let _ = b.generate(r).await.unwrap();

    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
    let content = messages[0].get("content").unwrap();
    // With cache_user_prefix, content MUST be an array of two blocks: cached prefix + volatile suffix.
    assert!(
        content.is_array(),
        "expected content to be a block array, got {content:?}"
    );
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(
        arr[0].get("text").and_then(|v| v.as_str()),
        Some("PREFIX_BLOCK")
    );
    assert_eq!(
        arr[0]
            .get("cache_control")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str()),
        Some("ephemeral"),
    );
    assert_eq!(arr[1].get("text").and_then(|v| v.as_str()), Some("suffix"));
    assert!(
        arr[1].get("cache_control").is_none(),
        "second block must NOT have cache_control"
    );
}

#[tokio::test]
async fn cache_user_prefix_in_middle_of_multibyte_codepoint_falls_back_to_plain_string() {
    // The character "中" is 3 bytes (E4 B8 AD). Setting prefix to byte 1 lands mid-codepoint.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let mut r = req("claude-haiku-4-5", "中文"); // 6 bytes, char boundaries at 0, 3, 6
    r.cache_user_prefix = Some(1); // mid "中" — must NOT panic
    let _ = b.generate(r).await.unwrap();
    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
    // Should fall through to plain-string content (no caching applied).
    assert!(
        messages[0].get("content").unwrap().is_string(),
        "mid-codepoint cache_user_prefix should fall back to plain-string content, not panic"
    );
}

#[tokio::test]
async fn no_caching_hints_keeps_legacy_request_shape() {
    // When neither cache_system nor cache_user_prefix is set, system stays
    // a plain string and content stays a plain string — minimizes JSON
    // shape churn for callers that don't need caching.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(r#"{"content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":10,"output_tokens":1}}"#),
            )
            .mount(&server)
            .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let mut r = req("claude-haiku-4-5", "hi");
    r.system = Some("you are a tester");
    // Both caching hints stay default (false / None) — same shape as before P3.
    let _ = b.generate(r).await.unwrap();

    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert!(
        body.get("system").unwrap().is_string(),
        "system should stay a string when not caching"
    );
    let messages = body.get("messages").and_then(|v| v.as_array()).unwrap();
    assert!(
        messages[0].get("content").unwrap().is_string(),
        "content should stay a string when not caching"
    );
}

#[tokio::test]
async fn happy_path_returns_text_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_x",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "hello world"}],
            "model": "claude-haiku-4-5",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 7}
        })))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "test-key", Duration::from_secs(5));
    let r = b.generate(req("claude-haiku-4-5", "hi")).await.unwrap();
    assert_eq!(r.text, "hello world");
    assert_eq!(r.usage.input_tokens, 5);
    assert_eq!(r.usage.output_tokens, 7);
    assert_eq!(r.usage.provider, "anthropic");
    assert_eq!(r.usage.model, "claude-haiku-4-5");
}

#[tokio::test]
async fn unauthorized_401_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        })))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
    let r = b.generate(req("claude-haiku-4-5", "hi")).await;
    let err = r.err().unwrap();
    let typed = err
        .downcast_ref::<BackendError>()
        .expect("typed BackendError");
    assert!(matches!(
        typed,
        BackendError::Unauthorized {
            provider: "anthropic"
        }
    ));
}

#[tokio::test]
async fn not_found_404_maps_to_model_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": {"type": "not_found_error", "message": "model not found"}
        })))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let r = b.generate(req("claude-bogus", "hi")).await;
    let err = r.err().unwrap();
    let typed = err
        .downcast_ref::<BackendError>()
        .expect("typed BackendError");
    match typed {
        BackendError::ModelNotFound { provider, model } => {
            assert_eq!(*provider, "anthropic");
            assert_eq!(model, "claude-bogus");
        }
        other => panic!("expected ModelNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn server_error_500_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let r = b.generate(req("claude-haiku-4-5", "hi")).await;
    let err = r.err().unwrap();
    let typed = err
        .downcast_ref::<BackendError>()
        .expect("typed BackendError");
    assert!(matches!(
        typed,
        BackendError::ServerError { status: 500, .. }
    ));
}

#[tokio::test]
async fn rate_limited_429_maps_to_typed_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let r = b.generate(req("claude-haiku-4-5", "hi")).await;
    let err = r.err().unwrap();
    let typed = err
        .downcast_ref::<BackendError>()
        .expect("typed BackendError");
    assert!(matches!(typed, BackendError::RateLimited { .. }));
}

#[tokio::test]
async fn opus_4_7_drops_temperature() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let _ = b.generate(req("claude-opus-4-7", "hi")).await.unwrap();

    // Verify the request body explicitly — this is more robust than
    // body_json matchers (which can be brittle to serde_json key ordering).
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        body.get("model").and_then(|v| v.as_str()),
        Some("claude-opus-4-7")
    );
    assert!(
        body.get("temperature").is_none(),
        "temperature should be dropped for Opus 4.7, but found: {:?}",
        body.get("temperature")
    );
    assert_eq!(
        body.get("thinking")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str()),
        Some("disabled")
    );
}

#[tokio::test]
async fn non_opus_4_7_keeps_temperature() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let _ = b.generate(req("claude-haiku-4-5", "hi")).await.unwrap();

    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(
        body.get("temperature").and_then(|v| v.as_f64()),
        Some(0.5),
        "temperature should be preserved for Haiku"
    );
}

#[tokio::test]
async fn streaming_happy_path_emits_text_deltas_then_final_usage() {
    use futures::StreamExt;
    let server = MockServer::start().await;
    let sse_body = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_x\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" \"}}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\
\n\
event: content_block_stop\n\
data: {\"type\":\"content_block_stop\",\"index\":0}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;

    let b = AnthropicBackend::new(&server.uri(), "test-key", Duration::from_secs(5));
    let mut stream = b
        .generate_stream(req("claude-haiku-4-5", "hi"))
        .await
        .unwrap();

    let mut text = String::new();
    let mut final_usage = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        text.push_str(&chunk.delta);
        if let Some(u) = chunk.usage {
            assert!(
                final_usage.is_none(),
                "usage should arrive only on final chunk"
            );
            final_usage = Some(u);
        }
    }
    assert_eq!(text, "Hello world");
    let u = final_usage.expect("expected final usage chunk");
    assert_eq!(u.input_tokens, 5);
    assert_eq!(u.output_tokens, 3);
    assert_eq!(u.provider, "anthropic");
    assert_eq!(u.model, "claude-haiku-4-5");
}

#[tokio::test]
async fn streaming_unauthorized_401_maps_to_typed_error_at_connect() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "bad-key", Duration::from_secs(5));
    let r = b.generate_stream(req("claude-haiku-4-5", "hi")).await;
    let err = r.err().unwrap();
    let typed = err
        .downcast_ref::<BackendError>()
        .expect("typed BackendError");
    assert!(matches!(
        typed,
        BackendError::Unauthorized {
            provider: "anthropic"
        }
    ));
}

#[tokio::test]
async fn streaming_rate_limited_429_maps_to_typed_error_at_connect() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let r = b.generate_stream(req("claude-haiku-4-5", "hi")).await;
    let err = r.err().unwrap();
    let typed = err
        .downcast_ref::<BackendError>()
        .expect("typed BackendError");
    assert!(matches!(typed, BackendError::RateLimited { .. }));
}

#[tokio::test]
async fn streaming_request_body_includes_stream_true() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"),
        )
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let _ = b
        .generate_stream(req("claude-haiku-4-5", "hi"))
        .await
        .unwrap();
    let received = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body.get("stream").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn streaming_handles_crlf_block_separators() {
    use futures::StreamExt;
    let server = MockServer::start().await;
    // Same body as happy-path but with \r\n line endings (some proxies emit CRLF).
    let sse_body = "\
event: content_block_delta\r\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\r\n\
\r\n\
event: message_delta\r\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\r\n\
\r\n\
event: message_stop\r\n\
data: {\"type\":\"message_stop\"}\r\n\
\r\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let mut stream = b
        .generate_stream(req("claude-haiku-4-5", "hi"))
        .await
        .unwrap();
    let mut text = String::new();
    let mut got_usage = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        text.push_str(&chunk.delta);
        if chunk.usage.is_some() {
            got_usage = true;
        }
    }
    assert_eq!(text, "hi");
    assert!(
        got_usage,
        "expected final usage chunk despite CRLF separators"
    );
}

#[tokio::test]
async fn streaming_salvages_final_usage_on_truncated_eof() {
    use futures::StreamExt;
    let server = MockServer::start().await;
    // message_delta arrives but the closing \n\n and message_stop are missing
    // (server cut connection mid-stream). The salvage path should still emit usage.
    let sse_body = "\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse_body),
        )
        .mount(&server)
        .await;
    let b = AnthropicBackend::new(&server.uri(), "k", Duration::from_secs(5));
    let mut stream = b
        .generate_stream(req("claude-haiku-4-5", "hi"))
        .await
        .unwrap();
    let mut final_usage = None;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        if let Some(u) = chunk.usage {
            final_usage = Some(u);
        }
    }
    let u = final_usage.expect("expected usage salvaged from truncated stream");
    assert_eq!(u.input_tokens, 7);
    assert_eq!(u.output_tokens, 2);
}

/// One real-API integration test gated on ANTHROPIC_API_KEY.
/// Run via `cargo test -- --ignored` only.
#[tokio::test]
#[ignore = "requires ANTHROPIC_API_KEY env var; costs ~$0.0001 per run"]
async fn live_anthropic_haiku_responds() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        panic!("ANTHROPIC_API_KEY must be set to run this --ignored test");
    };
    let b = AnthropicBackend::new("https://api.anthropic.com", &key, Duration::from_secs(30));
    let r = b
        .generate(ChatRequest {
            model: "claude-haiku-4-5",
            system: Some("You answer in exactly one short sentence."),
            user: "What is 2+2?",
            max_tokens: 32,
            temperature: Some(0.0),
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        })
        .await
        .expect("live API call should succeed");
    assert!(!r.text.is_empty());
    assert!(r.usage.input_tokens > 0);
    assert!(r.usage.output_tokens > 0);
    assert_eq!(r.usage.provider, "anthropic");
}
