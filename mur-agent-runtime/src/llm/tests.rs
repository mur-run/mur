use super::*;

#[test]
fn retry_after_parses_delta_seconds() {
    assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
    assert_eq!(parse_retry_after("  5  "), Some(Duration::from_secs(5)));
    assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
}

#[test]
fn retry_after_parses_http_date() {
    let future = chrono::Utc::now() + chrono::Duration::seconds(30);
    let header = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    let got = parse_retry_after(&header).expect("an HTTP-date is a valid retry-after");
    // The clock moves between formatting and parsing; 2s of slack keeps
    // this from flaking on a loaded machine.
    assert!(
        got >= Duration::from_secs(28) && got <= Duration::from_secs(30),
        "expected ~30s, got {got:?}"
    );
}

/// A date already in the past means "retry now", not "never" — the server
/// is entitled to send a stale one and we must not turn that into a hang.
#[test]
fn retry_after_past_date_is_zero() {
    assert_eq!(
        parse_retry_after("Sat, 01 Feb 2020 00:00:00 GMT"),
        Some(Duration::ZERO)
    );
}

#[test]
fn retry_after_garbage_is_none() {
    assert_eq!(parse_retry_after("soon"), None);
    assert_eq!(parse_retry_after(""), None);
    assert_eq!(parse_retry_after("   "), None);
    assert_eq!(parse_retry_after("-1"), None);
    assert_eq!(parse_retry_after("5.5"), None);
}

#[test]
fn retry_after_is_clamped() {
    assert_eq!(parse_retry_after("99999"), Some(RETRY_AFTER_MAX));
}

/// The delay is advice about *when* to retry, never about *whether*. A
/// 429 stays retry-then-advance no matter what the header said.
#[test]
fn classify_ignores_retry_after() {
    use Disposition::*;
    assert!(matches!(
        classify(&LlmError::RateLimit(None)),
        RetryThenAdvance
    ));
    assert!(matches!(
        classify(&LlmError::RateLimit(Some(Duration::from_secs(60)))),
        RetryThenAdvance
    ));
}

/// The error text reaches task JSON, where a human reads it. When the
/// server told us how long to wait, that number belongs in the message.
#[test]
fn rate_limit_renders_the_delay_when_known() {
    assert_eq!(LlmError::RateLimit(None).to_string(), "rate limit");
    assert_eq!(
        LlmError::RateLimit(Some(Duration::from_secs(42))).to_string(),
        "rate limit (retry after 42s)"
    );
}

#[test]
fn from_status_with_headers_reads_retry_after() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
    assert!(matches!(
        LlmError::from_status_with_headers(429, "x".into(), &headers),
        LlmError::RateLimit(Some(d)) if d == Duration::from_secs(7)
    ));
    // A header on a non-429 is not our business.
    assert!(matches!(
        LlmError::from_status_with_headers(503, "x".into(), &headers),
        LlmError::ServerError(503)
    ));
    // No header at all is the pre-existing behaviour, byte for byte.
    let empty = reqwest::header::HeaderMap::new();
    assert!(matches!(
        LlmError::from_status_with_headers(429, "x".into(), &empty),
        LlmError::RateLimit(None)
    ));
}

#[test]
fn rich_message_text_roundtrip() {
    let m = RichMessage::Text {
        role: "user".into(),
        content: "hello".into(),
    };
    match m {
        RichMessage::Text { role, content } => {
            assert_eq!(role, "user");
            assert_eq!(content, "hello");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn llm_request_tools_defaults_empty() {
    let req = LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: "hi".into(),
        }],
        temperature: None,
        max_tokens: None,
        tools: vec![],
        ..Default::default()
    };
    assert!(req.tools.is_empty());
}

#[test]
fn llm_request_intent_defaults_interactive() {
    let r = LlmRequest::default();
    assert_eq!(r.intent, RequestIntent::Interactive);
    assert!(r.pin_model_ref.is_none());
    assert!(r.task_id.is_none());
}

#[test]
fn llm_response_defaults() {
    let r = LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: "hello".into(),
        input_tokens: 5,
        output_tokens: 2,
        model: "claude-3".into(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
    };
    assert!(r.tool_calls.is_empty());
    assert_eq!(r.stop_reason, StopReason::EndTurn);
}

#[test]
fn from_status_maps_http_codes() {
    assert!(matches!(
        LlmError::from_status(429, "x".into()),
        LlmError::RateLimit(None)
    ));
    assert!(matches!(
        LlmError::from_status(402, "x".into()),
        LlmError::InsufficientCredit
    ));
    assert!(matches!(
        LlmError::from_status(503, "x".into()),
        LlmError::ServerError(503)
    ));
    // 400 is no longer the `Http` catch-all: an unrecognised 4xx is a
    // refusal by THIS endpoint and may well succeed on the next candidate.
    assert!(matches!(
        LlmError::from_status(400, "x".into()),
        LlmError::Rejected(400, _)
    ));
    assert!(matches!(
        LlmError::from_status(401, "x".into()),
        LlmError::Auth(401, _)
    ));
}

#[test]
fn transient_failures_retry_then_advance() {
    use Disposition::*;
    assert!(matches!(
        classify(&LlmError::RateLimit(None)),
        RetryThenAdvance
    ));
    assert!(matches!(classify(&LlmError::Timeout), RetryThenAdvance));
    assert!(matches!(
        classify(&LlmError::ServerError(500)),
        RetryThenAdvance
    ));
    assert!(matches!(
        classify(&LlmError::Connect("connection refused".into())),
        RetryThenAdvance
    ));
}

/// A missing model is proven candidate-specific; exhausted account credit
/// is not and must not silently route to a different billing path.
#[test]
fn only_proven_candidate_failures_advance_without_retrying() {
    use Disposition::*;
    assert!(matches!(
        classify(&LlmError::ModelNotFound("no such model".into())),
        AdvanceNow
    ));
    assert!(matches!(classify(&LlmError::InsufficientCredit), Stop));
}

/// Auth must never fall back. Presenting the same broken credential to a
/// second provider fails identically and buries a config error the operator
/// has to fix — and could spend money under a misconfiguration nobody asked
/// for.
#[test]
fn auth_and_malformed_stop_the_chain() {
    use Disposition::*;
    assert!(matches!(
        classify(&LlmError::Http("status 401: unauthorized".into())),
        Stop
    ));
    assert!(matches!(classify(&LlmError::Http("400".into())), Stop));
    assert!(matches!(
        classify(&LlmError::InvalidResponse("x".into())),
        Stop
    ));
}

/// 404 must not land in the `Http` catch-all, which is `Stop`. This is the
/// status a provider returns for a renamed or retired model id, and it is
/// precisely what a fallback chain exists to survive.
#[test]
fn a_404_becomes_model_not_found_not_a_generic_http_error() {
    let e = LlmError::from_status(404, "model claude-sonnet-4-6 not found".into());
    assert!(matches!(e, LlmError::ModelNotFound(_)), "{e:?}");
    assert_eq!(classify(&e), Disposition::AdvanceNow);
    // 401 shares the catch-all and must keep stopping.
    assert_eq!(
        classify(&LlmError::from_status(401, "unauthorized".into())),
        Disposition::Stop
    );
}

#[test]
fn from_status_maps_408_to_timeout() {
    assert!(matches!(
        LlmError::from_status(408, String::new()),
        LlmError::Timeout
    ));
    // Auth is the one closed set that must never advance the chain.
    for status in [401, 403] {
        let e = LlmError::from_status(status, "unauthorized".into());
        assert!(matches!(e, LlmError::Auth(..)), "{status}: {e:?}");
        assert_eq!(classify(&e), Disposition::Stop, "{status}");
    }
}

#[test]
fn fleet_wide_unknown_client_errors_and_credit_stop() {
    use Disposition::*;
    for status in [400, 409, 413, 422] {
        let error = LlmError::from_status(status, "provider-specific refusal".into());
        assert!(matches!(error, LlmError::Rejected(s, _) if s == status));
        assert_eq!(classify(&error), Stop, "status {status}");
    }
    assert_eq!(classify(&LlmError::InsufficientCredit), Stop);
}

#[test]
fn typed_request_failures_have_bounded_dispositions() {
    use Disposition::*;
    assert_eq!(
        classify(&LlmError::ContextExceeded("too many tokens".into())),
        AdvanceNow
    );
    assert_eq!(
        classify(&LlmError::PermissionDenied(403, "denied".into())),
        Stop
    );
    assert_eq!(
        classify(&LlmError::SafetyPolicyRejected("unsafe".into())),
        Stop
    );
}
