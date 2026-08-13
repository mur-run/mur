use super::super::BackgroundKind;
use super::*;

#[test]
fn cooldown_marks_and_expires() {
    let cm = CooldownMap::new();
    let now = Instant::now();
    assert!(!cm.is_cooling("m", now));
    cm.mark("m", now + Duration::from_secs(60));
    assert!(cm.is_cooling("m", now)); // within window
    assert!(!cm.is_cooling("m", now + Duration::from_secs(61))); // after window
    assert!(!cm.is_cooling("other", now));
}

#[test]
fn backoff_grows_and_stays_in_bounds() {
    let base = 500u64;
    for attempt in 0..4u32 {
        let d = backoff_delay(attempt, base).as_millis() as u64;
        let floor = base * 2u64.pow(attempt);
        assert!(
            d >= floor && d < floor + base,
            "attempt {attempt}: {d} not in [{floor}, {})",
            floor + base
        );
    }
}

#[test]
fn estimate_tokens_sums_text_over_rich_messages() {
    let req = LlmRequest {
        messages: vec![
            RichMessage::Text {
                role: "user".into(),
                content: "a".repeat(40),
            },
            RichMessage::ImageText {
                role: "user".into(),
                media_type: "image/png".into(),
                data: String::new(),
                text: "b".repeat(40),
            },
        ],
        temperature: None,
        max_tokens: None,
        tools: vec![],
        ..Default::default()
    };
    assert_eq!(estimate_input_tokens(&req), 20); // 80 chars / 4
}

use super::super::StopReason;
use std::sync::atomic::{AtomicUsize, Ordering};

// LlmResponse has no Default (StopReason has no default variant), so build
// one explicitly.
fn mk_resp(text: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_string(),
        input_tokens: 0,
        output_tokens: 0,
        model: text.to_string(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
    }
}

// Mock client whose Nth generate() outcome is scripted.
struct ScriptClient {
    name: String,
    outcomes: Vec<Result<(), LlmError>>,
    idx: AtomicUsize,
}
#[async_trait]
impl LlmClient for ScriptClient {
    async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let i = self
            .idx
            .fetch_add(1, Ordering::SeqCst)
            .min(self.outcomes.len() - 1);
        match &self.outcomes[i] {
            Ok(()) => Ok(mk_resp(&self.name)),
            Err(e) => Err(e.clone()),
        }
    }
    fn model_name(&self) -> &str {
        &self.name
    }
}

fn factory_for(scripts: HashMap<String, Vec<Result<(), LlmError>>>) -> ClientFactory {
    Box::new(move |r: &str| {
        let o = scripts.get(r).cloned().unwrap_or_else(|| vec![Ok(())]);
        Ok(Arc::new(ScriptClient {
            name: r.to_string(),
            outcomes: o,
            idx: AtomicUsize::new(0),
        }) as Arc<dyn LlmClient>)
    })
}

fn retry0() -> RetryConfig {
    RetryConfig {
        max_retries: 0,
        backoff_base_ms: 1,
        cooldown_secs: 60,
    }
}

#[tokio::test]
async fn advances_chain_on_retryable_then_succeeds() {
    let mut s = HashMap::new();
    s.insert("a".into(), vec![Err(LlmError::ServerError(500))]); // a fails (retryable)
    s.insert("b".into(), vec![Ok(())]); // b succeeds
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
    let resp = fb.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.text, "b"); // fell through to b
}

#[tokio::test]
async fn connect_error_advances_chain() {
    // Transport failure (endpoint unreachable — e.g. a dead local proxy) must
    // advance the chain: the server rendered no verdict, so this is not the
    // auth/misconfig class that stays Fatal.
    let mut s = HashMap::new();
    s.insert(
        "a".into(),
        vec![Err(LlmError::Connect("connection refused".into()))],
    );
    s.insert("b".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
    let resp = fb.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.text, "b");
}

#[tokio::test]
async fn fatal_error_does_not_advance() {
    let mut s = HashMap::new();
    s.insert("a".into(), vec![Err(LlmError::Http("401".into()))]); // fatal
    s.insert("b".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
    let err = fb.generate(LlmRequest::default()).await.unwrap_err();
    assert!(matches!(err, LlmError::Http(_))); // returned a's fatal error, never tried b
}

/// A renamed or retired model id is exactly what a fallback chain is for. It
/// used to be lumped into the `Http` catch-all and stop the turn outright,
/// leaving a perfectly good fallback unused.
#[tokio::test]
async fn a_model_not_found_advances_to_the_next_candidate() {
    let mut s = HashMap::new();
    s.insert(
        "a".into(),
        vec![Err(LlmError::ModelNotFound("claude-sonnet-4-6".into()))],
    );
    s.insert("b".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
    let resp = fb.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.text, "b", "the chain must survive a retired model id");
}

/// ...and it must advance WITHOUT spending the retry budget. A 404 cannot
/// become a 200 by asking the same endpoint again, so backing off against it
/// is pure latency. The script makes the difference observable: candidate `a`
/// would succeed on its second call, so any retry at all returns "a".
#[tokio::test]
async fn a_model_not_found_does_not_burn_retries_on_the_same_candidate() {
    let retry3 = RetryConfig {
        max_retries: 3,
        backoff_base_ms: 1,
        cooldown_secs: 60,
    };
    let mut s = HashMap::new();
    s.insert(
        "a".into(),
        vec![Err(LlmError::ModelNotFound("gone".into())), Ok(())],
    );
    s.insert("b".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry3);
    let resp = fb.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(
        resp.text, "b",
        "a retry would have hit a's scripted Ok and returned \"a\""
    );
}

/// Control for the two above: auth still stops the chain dead. Falling back
/// here would re-present the same broken credential to a second provider and
/// bury a config error the operator has to fix.
#[tokio::test]
async fn auth_still_does_not_advance() {
    let mut s = HashMap::new();
    s.insert("a".into(), vec![Err(LlmError::Http("status 401".into()))]);
    s.insert("b".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
    let err = fb.generate(LlmRequest::default()).await.unwrap_err();
    assert!(matches!(err, LlmError::Http(_)), "never tried b");
}

#[tokio::test]
async fn exhaustion_returns_last_error() {
    let mut s = HashMap::new();
    s.insert("a".into(), vec![Err(LlmError::RateLimit)]);
    s.insert("b".into(), vec![Err(LlmError::ServerError(503))]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
    let err = fb.generate(LlmRequest::default()).await.unwrap_err();
    assert!(matches!(err, LlmError::ServerError(503))); // last candidate's error
}

#[test]
fn candidates_pin_overrides_everything() {
    // A pinned ref returns exactly [pinned] regardless of source.
    let fb = FallbackLlmClient::new(
        vec!["a".into(), "b".into()],
        factory_for(Default::default()),
        retry0(),
    );
    let req = LlmRequest {
        pin_model_ref: Some("frontier".into()),
        ..Default::default()
    };
    assert_eq!(fb.candidates_for(&req), vec!["frontier".to_string()]);
}

#[test]
fn candidates_smart_background_prepends_cheap() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, SmartConfig};
    let cfg = ModelSwitchConfig {
        default: Some("primary".into()),
        fallback_chain: vec!["primary".into(), "mid".into()],
        smart: SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        },
        ..Default::default()
    };
    let fb = FallbackLlmClient::new_routed(
        AgentProfile::default_for_tests(),
        cfg,
        factory_for(Default::default()),
        retry0(),
    );
    // Background + smart → cheap first, then phase-1 candidates, deduped.
    let bg = LlmRequest {
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
        ..Default::default()
    };
    assert_eq!(
        fb.candidates_for(&bg),
        vec!["cheap".to_string(), "primary".into(), "mid".into()]
    );
    // Interactive → unchanged (no cheap prepend).
    let inter = LlmRequest::default();
    assert_eq!(
        fb.candidates_for(&inter),
        vec!["primary".to_string(), "mid".into()]
    );
}

#[tokio::test]
async fn routed_generate_picks_frontier_for_large_request() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, RoutingConfig};
    let cfg = ModelSwitchConfig {
        routing: RoutingConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            frontier: Some("frontier".into()),
            threshold_input_tokens: Some(5),
            smart: None,
        },
        ..Default::default()
    };
    let mut scripts = std::collections::HashMap::new();
    scripts.insert("frontier".to_string(), vec![Ok(())]);
    scripts.insert("cheap".to_string(), vec![Ok(())]);
    let fb = FallbackLlmClient::new_routed(
        AgentProfile::default_for_tests(),
        cfg,
        factory_for(scripts),
        retry0(),
    );
    // A big request (> threshold=5 tokens) routes to frontier.
    let big = LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: "x".repeat(400),
        }],
        temperature: None,
        max_tokens: None,
        tools: vec![],
        ..Default::default()
    };
    assert_eq!(fb.generate(big).await.unwrap().text, "frontier");
    // A tiny request routes to cheap.
    let small = LlmRequest {
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: "x".into(),
        }],
        temperature: None,
        max_tokens: None,
        tools: vec![],
        ..Default::default()
    };
    assert_eq!(fb.generate(small).await.unwrap().text, "cheap");
}

#[tokio::test]
async fn cascade_escalates_structural_fail_under_background_smart() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, SmartConfig};
    // cheap returns InvalidResponse (structural), then primary succeeds.
    let mut scripts = std::collections::HashMap::new();
    scripts.insert(
        "cheap".to_string(),
        vec![Err(LlmError::InvalidResponse("empty".into()))],
    );
    scripts.insert("primary".to_string(), vec![Ok(())]);

    let cfg = ModelSwitchConfig {
        default: Some("primary".into()),
        smart: SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        },
        ..Default::default()
    };
    let fb = FallbackLlmClient::new_routed(
        AgentProfile::default_for_tests(),
        cfg,
        factory_for(scripts),
        retry0(),
    );

    let bg = LlmRequest {
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
        ..Default::default()
    };
    assert_eq!(fb.generate(bg).await.unwrap().text, "primary");
}

#[tokio::test]
async fn interactive_invalid_response_stays_fatal() {
    let mut scripts = std::collections::HashMap::new();
    scripts.insert(
        "a".to_string(),
        vec![Err(LlmError::InvalidResponse("x".into()))],
    );
    scripts.insert("b".to_string(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(scripts), retry0());
    // Interactive: InvalidResponse is Fatal → returns the error, never tries b.
    let err = fb.generate(LlmRequest::default()).await.unwrap_err();
    assert!(matches!(err, LlmError::InvalidResponse(_)));
}

#[tokio::test]
async fn cascade_respects_max_escalations() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, SmartConfig};
    // Both cheap AND primary return InvalidResponse; max_escalations=1
    // means only one escalation is allowed, so the second structural
    // failure (on primary) must surface instead of looping past the cap.
    let mut scripts = std::collections::HashMap::new();
    scripts.insert(
        "cheap".to_string(),
        vec![Err(LlmError::InvalidResponse("empty".into()))],
    );
    scripts.insert(
        "primary".to_string(),
        vec![Err(LlmError::InvalidResponse("still empty".into()))],
    );

    let cfg = ModelSwitchConfig {
        default: Some("primary".into()),
        smart: SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        },
        ..Default::default()
    };
    let fb = FallbackLlmClient::new_routed(
        AgentProfile::default_for_tests(),
        cfg,
        factory_for(scripts),
        retry0(),
    );

    let bg = LlmRequest {
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
        ..Default::default()
    };
    let err = fb.generate(bg).await.unwrap_err();
    assert!(matches!(err, LlmError::InvalidResponse(_)));
}

#[tokio::test]
async fn generate_without_telemetry_never_panics() {
    // Every fixture above builds via `new`/`new_routed` with no
    // `with_telemetry` call — `telemetry` stays `None`, so this just
    // re-confirms the no-sink path is inert (no panic, normal result).
    let mut s = HashMap::new();
    s.insert("a".into(), vec![Ok(())]);
    let fb = FallbackLlmClient::new(vec!["a".into()], factory_for(s), retry0());
    let resp = fb.generate(LlmRequest::default()).await.unwrap();
    assert_eq!(resp.text, "a");
}

#[tokio::test]
async fn routed_generate_emits_routing_event_on_escalation() {
    use mur_common::agent::AgentProfile;
    use mur_common::config::{ModelSwitchConfig, SmartConfig};
    // cheap structurally fails once, escalates to primary which succeeds
    // — exercises the "escalated" outcome + attempts/escalations counts
    // end-to-end through the real with_telemetry wiring.
    let mut scripts = HashMap::new();
    scripts.insert(
        "cheap".to_string(),
        vec![Err(LlmError::InvalidResponse("empty".into()))],
    );
    scripts.insert("primary".to_string(), vec![Ok(())]);

    let cfg = ModelSwitchConfig {
        default: Some("primary".into()),
        smart: SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        },
        ..Default::default()
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let fb = FallbackLlmClient::new_routed(
        AgentProfile::default_for_tests(),
        cfg,
        factory_for(scripts),
        retry0(),
    )
    .with_telemetry(tx, "coach");

    let bg = LlmRequest {
        intent: RequestIntent::Background(BackgroundKind::Scheduled),
        messages: vec![RichMessage::Text {
            role: "user".into(),
            content: "do the thing".into(),
        }],
        ..Default::default()
    };
    let resp = fb.generate(bg).await.unwrap();
    assert_eq!(resp.text, "primary");

    let ev = rx.try_recv().expect("routing event emitted");
    match ev {
        Event::Routing {
            agent,
            intent,
            model_ref,
            reason,
            outcome,
            attempts,
            escalations,
            task_summary,
            ..
        } => {
            assert_eq!(agent, "coach");
            assert_eq!(intent, "background/scheduled");
            assert_eq!(model_ref, "primary");
            assert_eq!(reason, "smart-background");
            assert_eq!(outcome, "escalated");
            assert_eq!(attempts, 2);
            assert_eq!(escalations, 1);
            assert_eq!(task_summary, "do the thing");
        }
        other => panic!("expected Event::Routing, got {other:?}"),
    }
}
