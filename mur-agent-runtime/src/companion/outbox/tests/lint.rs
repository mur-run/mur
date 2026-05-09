//! M5.4 lint tests — generate + lint with one regenerate, persistent linter
//! failure, and notifier failure.

use super::*;

// M5.4 Test 1: clean body → MessageScheduled → MessageGenerated(regen=0) → MessageSent
#[tokio::test]
async fn linter_pass_emits_generated_then_sent() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    let now_utc = clock.now_utc();
    let now_local = clock.now_local();
    let outcome = outbox.run_tick(now_utc, now_local).await;

    assert!(
        matches!(outcome, TickOutcome::Sent { .. }),
        "expected Sent, got {outcome:?}"
    );
    assert_eq!(
        notifier.call_count(),
        1,
        "notifier should have been called once"
    );

    let events = all_events(tmp.path());
    let has_scheduled = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageScheduled { .. }));
    let has_generated = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageGenerated { regen_count: 0, .. }));
    let has_sent = events
        .iter()
        .any(|e| matches!(e, OutboxEvent::MessageSent { .. }));

    assert!(
        has_scheduled,
        "must have MessageScheduled; events: {events:?}"
    );
    assert!(
        has_generated,
        "must have MessageGenerated(regen_count=0); events: {events:?}"
    );
    assert!(has_sent, "must have MessageSent; events: {events:?}");
}

// M5.4 Test 2: first generate → banned body → regenerate → clean body → MessageSent
#[tokio::test]
async fn linter_first_fail_regenerates_then_passes() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_bad_then_clean_zh(),
        notifier.clone(),
    );

    let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(outcome, TickOutcome::Sent { .. }),
        "expected Sent after regenerate, got {outcome:?}"
    );
    assert_eq!(notifier.call_count(), 1);

    let events = all_events(tmp.path());

    // Must have TWO MessageGenerated events: regen_count 0 then 1.
    let generated_events: Vec<&OutboxEvent> = events
        .iter()
        .filter(|e| matches!(e, OutboxEvent::MessageGenerated { .. }))
        .collect();
    assert_eq!(
        generated_events.len(),
        2,
        "expected 2 MessageGenerated events; got: {generated_events:?}"
    );
    assert!(
        matches!(
            generated_events[0],
            OutboxEvent::MessageGenerated { regen_count: 0, .. }
        ),
        "first MessageGenerated must have regen_count=0"
    );
    assert!(
        matches!(
            generated_events[1],
            OutboxEvent::MessageGenerated { regen_count: 1, .. }
        ),
        "second MessageGenerated must have regen_count=1"
    );

    // Final event is MessageSent.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
        "must end with MessageSent"
    );
}

// M5.4 Test 3: banned body twice → MessageDropped(linter_persistent)
#[tokio::test]
async fn linter_persistent_drops() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_always_banned_zh(),
        notifier.clone(),
    );

    let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(
            outcome,
            TickOutcome::Skipped {
                reason: SkipReason::LinterPersistent
            }
        ),
        "expected LinterPersistent, got {outcome:?}"
    );
    assert_eq!(notifier.call_count(), 0, "notifier must not be called");

    let events = all_events(tmp.path());

    let generated_events: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, OutboxEvent::MessageGenerated { .. }))
        .collect();
    assert_eq!(
        generated_events.len(),
        2,
        "expected 2 MessageGenerated events for both attempts"
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            OutboxEvent::MessageDropped { reason, .. } if reason == "linter_persistent"
        )),
        "must have MessageDropped(linter_persistent)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
        "must NOT have MessageSent"
    );
}

// M5.4 Test 4: notifier fails → MessageDropped(notifier_failed)
#[tokio::test]
async fn notifier_failed_drops() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 5, None, None);
    let notifier = Arc::new(FakeNotifier::failed());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(
            outcome,
            TickOutcome::Skipped {
                reason: SkipReason::NotifierFailed
            }
        ),
        "expected NotifierFailed, got {outcome:?}"
    );

    let events = all_events(tmp.path());
    assert!(
        events.iter().any(|e| matches!(
            e,
            OutboxEvent::MessageDropped { reason, .. } if reason == "notifier_failed"
        )),
        "must have MessageDropped(notifier_failed)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, OutboxEvent::MessageSent { .. })),
        "must NOT have MessageSent"
    );
}
