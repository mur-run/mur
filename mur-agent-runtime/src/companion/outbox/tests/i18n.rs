//! M5.5 i18n tests — translate, 429 backoff resume, 4-failure terminal drop,
//! and generation rate-limit pause.

use super::*;
use chrono::Duration;

// M5.5 Test 1: mixed-language body (passes linter, fails heuristic_matches),
//              locale = zh-TW → stub returns translated zh-TW body → notifier
//              sees translated body.
//
// We use "Ok, 好。" as the generated body because:
//   - linter: "Ok," has comma so is NOT an all-ascii-alpha token; 0/2 English
//     tokens = 0% → passes PreservedEnglishRatioZh.  1 sentence → passes.
//   - heuristic_matches: CJK char "好" = 1 out of 5 non-whitespace = 20% < 30%
//     → fails → translation is triggered.
#[tokio::test]
async fn i18n_force_english_in_zh_locale_translates_then_sends() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());

    // Stub: generate returns a mixed body that passes the linter but triggers
    // translation (CJK ratio too low for zh-TW).  Translate call returns
    // the full zh-TW body.
    let translated = "早安！今天想從哪一件小事開始？";
    let llm = Arc::new(
        StubLlm::from_yaml(&format!(
            r#"
- match:
    contains: "Translate the following"
  response: "{translated}"
- match:
    contains: "situation:"
  response: "Ok, 好。"
"#
        ))
        .unwrap(),
    );

    let mut outbox = Outbox::with_picker(
        clock.clone(),
        ledger,
        picker,
        proactive,
        OutboxConfig {
            llm,
            notifier: notifier.clone(),
            voice_md: "You are a warm companion.".to_string(),
            locale: "zh-TW".to_string(),
            prompt_seeds: BTreeMap::new(),
            name_for_user: String::new(),
            first_memory: None,
            formality: String::new(),
            extra_instructions: String::new(),
            relationship: mur_common::companion::Relationship::Friend,
        },
    );

    let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(outcome, TickOutcome::Sent { .. }),
        "expected Sent, got {outcome:?}"
    );
    assert_eq!(notifier.call_count(), 1);
    assert_eq!(
        notifier.last_body().as_deref(),
        Some(translated),
        "notifier should have received translated body"
    );

    let events = all_events(tmp.path());
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageScheduled { .. }))
    );
    // MessageGenerated should have the English body sha256 (pre-translation).
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageGenerated { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageSent { .. }))
    );
}

// M5.5 Test 2: generate succeeds (mixed body that triggers translation),
//              translate → RateLimit on tick 1; tick 2 (after resume_at):
//              translate succeeds → MessageSent.
#[tokio::test]
async fn translate_429_pauses_then_resumes_after_resume_at() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());

    let translated = "早安！今天想從哪一件小事開始？";

    // Use a counter to flip behavior: first translate attempt → RateLimit,
    // subsequent → success.  We use a shared Mutex<u32> inside a custom LLM.
    use crate::llm::{LlmError, LlmRequest, LlmResponse};

    struct FlipLlm {
        translate_calls: Mutex<u32>,
        translated: String,
    }

    #[async_trait]
    impl LlmClient for FlipLlm {
        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let joined: String = req
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if joined.contains("Translate the following") {
                let mut calls = self.translate_calls.lock().unwrap();
                *calls += 1;
                if *calls == 1 {
                    return Err(LlmError::RateLimit);
                }
                return Ok(LlmResponse {
                    text: self.translated.clone(),
                    input_tokens: 0,
                    output_tokens: 0,
                    model: "flip".into(),
                });
            }
            // Generation call: return a body that passes linter but triggers
            // translation (CJK ratio < 30% for zh-TW).
            Ok(LlmResponse {
                text: "Ok, 好。".into(),
                input_tokens: 0,
                output_tokens: 0,
                model: "flip".into(),
            })
        }
        fn model_name(&self) -> &str {
            "flip"
        }
    }

    let llm: Arc<dyn LlmClient> = Arc::new(FlipLlm {
        translate_calls: Mutex::new(0),
        translated: translated.to_string(),
    });

    let mut outbox = Outbox::with_picker(
        clock.clone(),
        ledger,
        picker,
        proactive,
        OutboxConfig {
            llm,
            notifier: notifier.clone(),
            voice_md: "You are a warm companion.".to_string(),
            locale: "zh-TW".to_string(),
            prompt_seeds: BTreeMap::new(),
            name_for_user: String::new(),
            first_memory: None,
            formality: String::new(),
            extra_instructions: String::new(),
            relationship: mur_common::companion::Relationship::Friend,
        },
    );

    // Tick 1: translate fails → paused.
    let outcome1 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(
            outcome1,
            TickOutcome::Skipped {
                reason: SkipReason::LocaleUnresolved
            }
        ),
        "tick 1 should pause on translate failure; got {outcome1:?}"
    );
    assert_eq!(
        notifier.call_count(),
        0,
        "notifier must not be called on tick 1"
    );

    // Ledger must have MessagePaused but no MessageSent.
    {
        let events = all_events(tmp.path());
        assert!(
            events
                .iter()
                .any(|e| matches!(e, OutboxEvent::MessagePaused { .. })),
            "must have MessagePaused after tick 1"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
            "must NOT have MessageSent after tick 1"
        );
    }

    // Advance clock past the first backoff (30 s).
    clock.advance(Duration::seconds(31));

    // Tick 2: resume_at has elapsed; translate succeeds → sent.
    let outcome2 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(outcome2, TickOutcome::Sent { .. }),
        "tick 2 should succeed; got {outcome2:?}"
    );
    assert_eq!(notifier.call_count(), 1);
    assert_eq!(
        notifier.last_body().as_deref(),
        Some(translated),
        "notifier should have received translated body"
    );
}

// M5.5 Test 3: generate succeeds; translate always → RateLimit; 4 ticks →
//              MessageDropped(locale_unresolved) + LocaleMismatchUnresolved{attempts:4}.
#[tokio::test]
async fn translate_4_failures_drops_locale_unresolved() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 10, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());

    use crate::llm::{LlmError, LlmRequest, LlmResponse};

    struct AlwaysRateLimitTranslate;

    #[async_trait]
    impl LlmClient for AlwaysRateLimitTranslate {
        async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let joined: String = req
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if joined.contains("Translate the following") {
                return Err(LlmError::RateLimit);
            }
            // Generation returns a body that passes the linter but triggers
            // translation (CJK ratio < 30% for zh-TW).
            Ok(LlmResponse {
                text: "Ok, 好。".into(),
                input_tokens: 0,
                output_tokens: 0,
                model: "always-rl".into(),
            })
        }
        fn model_name(&self) -> &str {
            "always-rl"
        }
    }

    let llm: Arc<dyn LlmClient> = Arc::new(AlwaysRateLimitTranslate);

    let mut outbox = Outbox::with_picker(
        clock.clone(),
        ledger,
        picker,
        proactive,
        OutboxConfig {
            llm,
            notifier: notifier.clone(),
            voice_md: "You are a warm companion.".to_string(),
            locale: "zh-TW".to_string(),
            prompt_seeds: BTreeMap::new(),
            name_for_user: String::new(),
            first_memory: None,
            formality: String::new(),
            extra_instructions: String::new(),
            relationship: mur_common::companion::Relationship::Friend,
        },
    );

    // Backoff schedule: [30s, 90s, 240s, 900s].
    // Tick 1: new send → translate fails → pause(attempt=0, backoff=30s).
    let outcome1 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(
            outcome1,
            TickOutcome::Skipped {
                reason: SkipReason::LocaleUnresolved
            }
        ),
        "tick 1: {outcome1:?}"
    );

    // Advance past 30s → tick 2: resume → translate fails again → pause(attempt=1, backoff=90s).
    clock.advance(Duration::seconds(35));
    let outcome2 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    // Tick 2 may Sent a new message or Skipped — what matters is the resumed
    // attempt fails again.  We check ledger at the end.
    let _ = outcome2;

    // Advance past 90s → tick 3: resume → translate fails → pause(attempt=2, backoff=240s).
    clock.advance(Duration::seconds(95));
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    // Advance past 240s → tick 4: resume → translate fails → attempt=3 → backoff=900s.
    clock.advance(Duration::seconds(245));
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    // Advance past 900s → tick 5: resume → translate fails → attempt=4 → TERMINAL drop.
    clock.advance(Duration::seconds(905));
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    // Check ledger.
    let events = all_events(tmp.path());
    assert!(
        events.iter().any(|e| matches!(
            e,
            OutboxEvent::MessageDropped { reason, .. } if reason == "locale_unresolved"
        )),
        "must have MessageDropped(locale_unresolved); events: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OutboxEvent::LocaleMismatchUnresolved { attempts: 4, .. })),
        "must have LocaleMismatchUnresolved{{attempts: 4}}; events: {events:?}"
    );
    assert_eq!(notifier.call_count(), 0, "notifier must never be called");
}

// M5.5 Test 4: generate stub → RateLimit → ledger has MessagePaused{reason:rate_limit_429};
//              outcome = Skipped{PausedRateLimit}; sent_today unchanged.
#[tokio::test]
async fn llm_rate_limit_pauses_send() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());

    let llm = Arc::new(
        StubLlm::from_yaml(
            r#"
- match:
    contains: "situation:"
  fault: rate_limit
"#,
        )
        .unwrap(),
    );

    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        llm,
        notifier.clone(),
    );

    let now_utc = clock.now_utc();
    let outcome = outbox.run_tick(now_utc, clock.now_local()).await;

    // Verify outcome is PausedRateLimit.
    let resume_at = match &outcome {
        TickOutcome::Skipped {
            reason: SkipReason::PausedRateLimit { resume_at },
        } => *resume_at,
        other => panic!("expected PausedRateLimit, got {other:?}"),
    };
    assert!(
        resume_at > now_utc,
        "resume_at must be in the future; resume_at={resume_at}, now={now_utc}"
    );

    // sent_today must not have advanced.
    assert_eq!(outbox.sent_today, 0, "sent_today must be unchanged");

    // Ledger must have MessagePaused(rate_limit_429) but no MessageSent.
    let events = all_events(tmp.path());
    assert!(
        events.iter().any(|e| matches!(
            e,
            OutboxEvent::MessagePaused { reason, .. } if reason == "rate_limit_429"
        )),
        "must have MessagePaused(rate_limit_429); events: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
        "must NOT have MessageSent"
    );
}
