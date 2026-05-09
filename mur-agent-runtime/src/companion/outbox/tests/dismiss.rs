//! M5.6 passive-dismiss tests — sweep fires after 24h, skips within 24h /
//! when acked, and is idempotent.

use super::*;
use chrono::Duration;
use mur_common::companion::Signal;

// Helper: drive one tick and return the sent message id + template_id.
async fn drive_one_sent_tick(
    outbox: &mut Outbox<rand::rngs::StdRng>,
    clock: &Arc<MockClock>,
) -> (String, String) {
    let outcome = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    match outcome {
        TickOutcome::Sent {
            id, template_id, ..
        } => (id, template_id),
        other => panic!("expected Sent, got {other:?}"),
    }
}

// M5.6 Test 1: passive_dismiss_fires_after_24h
//
// Send at T. Advance 24h+1min. Tick again. Ledger has PassiveDismissInferred
// for the first id. Picker has dismiss_count > 0 for that template.
#[tokio::test]
async fn passive_dismiss_fires_after_24h() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 10, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    // Tick 1: get a successful send.
    let (sent_id, template_id) = drive_one_sent_tick(&mut outbox, &clock).await;

    // Confirm no PassiveDismissInferred yet.
    {
        let events = all_events(tmp.path());
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, OutboxEvent::PassiveDismissInferred { .. })),
            "should not have PassiveDismissInferred before 24h"
        );
    }

    // Advance > 24h so the sweep fires.
    clock.advance(Duration::hours(24) + Duration::minutes(1));

    // Tick 2: sweep fires. May also schedule a new send — doesn't matter.
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    // Ledger must contain a PassiveDismissInferred for `sent_id`.
    let events = all_events(tmp.path());
    let dismiss_ev = events.iter().find(|e| match e {
        OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
        _ => false,
    });
    assert!(
        dismiss_ev.is_some(),
        "PassiveDismissInferred must exist for id={sent_id}; events: {events:?}"
    );

    // Picker must have recorded a Dismiss for the template.
    let dismiss_count = outbox
        .picker
        .state
        .templates
        .get(&template_id)
        .map(|t| t.dismiss_count)
        .unwrap_or(0);
    assert!(
        dismiss_count > 0,
        "picker dismiss_count must be > 0 for template={template_id}"
    );
}

// M5.6 Test 2: passive_dismiss_skips_within_24h
//
// Send at T. Advance 23h 59min. Tick. No PassiveDismissInferred.
#[tokio::test]
async fn passive_dismiss_skips_within_24h() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 10, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    // Tick 1: send at T.
    let (sent_id, _) = drive_one_sent_tick(&mut outbox, &clock).await;

    // Advance only 23h 59min — still within the 24h window.
    clock.advance(Duration::hours(23) + Duration::minutes(59));

    // Tick 2: sweep must NOT fire.
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    let events = all_events(tmp.path());
    let dismiss_for_id = events.iter().any(|e| match e {
        OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
        _ => false,
    });
    assert!(
        !dismiss_for_id,
        "PassiveDismissInferred must NOT exist within 24h; events: {events:?}"
    );
}

// M5.6 Test 3: passive_dismiss_skips_when_acked
//
// Send at T. Manually append UserSignal{id, signal: Signal::Sent, at: T+1min}.
// Advance 25h. Tick. No PassiveDismissInferred.
#[tokio::test]
async fn passive_dismiss_skips_when_acked() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 10, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    // Tick 1: send at T.
    let (sent_id, _) = drive_one_sent_tick(&mut outbox, &clock).await;

    // Manually append a UserSignal to simulate that the user acknowledged.
    let ack_at = clock.now_utc() + Duration::minutes(1);
    let _ = outbox.ledger.append(&OutboxEvent::UserSignal {
        id: sent_id.clone(),
        signal: Signal::Positive,
        at: ack_at,
    });

    // Advance 25h.
    clock.advance(Duration::hours(25));

    // Tick 2: sweep must skip due to ack.
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    let events = all_events(tmp.path());
    let dismiss_for_id = events.iter().any(|e| match e {
        OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
        _ => false,
    });
    assert!(
        !dismiss_for_id,
        "PassiveDismissInferred must NOT exist for acked message; events: {events:?}"
    );
}

// M5.6 Test 4: passive_dismiss_idempotent
//
// Send at T. Advance 25h. Tick (sweep fires once). Tick again (no time
// advance). Ledger has exactly ONE PassiveDismissInferred for that id.
#[tokio::test]
async fn passive_dismiss_idempotent() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 10, None, None);
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    // Tick 1: send at T.
    let (sent_id, _) = drive_one_sent_tick(&mut outbox, &clock).await;

    // Advance 25h so sweep is eligible.
    clock.advance(Duration::hours(25));

    // Tick 2: sweep fires — first PassiveDismissInferred written.
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    // Count PassiveDismissInferred for sent_id after tick 2.
    let count_after_tick2 = all_events(tmp.path())
        .into_iter()
        .filter(|e| match e {
            OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
            _ => false,
        })
        .count();
    assert_eq!(
        count_after_tick2, 1,
        "expected exactly 1 PassiveDismissInferred after first sweep"
    );

    // Tick 3: same time — sweep must see existing PassiveDismissInferred in
    // acked_ids and not write a second one.
    let _ = outbox.run_tick(clock.now_utc(), clock.now_local()).await;

    let count_after_tick3 = all_events(tmp.path())
        .into_iter()
        .filter(|e| match e {
            OutboxEvent::PassiveDismissInferred { id, .. } => id == &sent_id,
            _ => false,
        })
        .count();
    assert_eq!(
        count_after_tick3, 1,
        "PassiveDismissInferred must be idempotent — still exactly 1 after second sweep"
    );
}
