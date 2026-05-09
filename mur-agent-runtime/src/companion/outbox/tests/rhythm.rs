//! M5.3 rhythm tests — proactive disabled, daily cap, morning greeting,
//! day rollover.  Must remain green after M5.4/M5.5 changes.

use super::*;
use chrono::Duration;

// Test 1: proactive disabled → 24 h sim yields 0 scheduled
#[tokio::test]
async fn proactive_disabled_runs_24h_zero_scheduled() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 8, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 42);
    let proactive = make_proactive(false, 3, None, None);
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        Arc::new(FakeNotifier::delivered()),
    );

    let ticks = 24 * 60;
    for _ in 0..ticks {
        let now_utc = clock.now_utc();
        let now_local = clock.now_local();
        let outcome = outbox.run_tick(now_utc, now_local).await;
        assert!(
            matches!(
                outcome,
                TickOutcome::Skipped {
                    reason: SkipReason::GateBlocked(BlockReason::ProactiveDisabled)
                }
            ),
            "expected ProactiveDisabled, got {outcome:?}"
        );
        clock.advance(Duration::seconds(60));
    }

    assert_eq!(
        count_scheduled(tmp.path()),
        0,
        "ledger must have zero MessageScheduled events when proactive is disabled"
    );
}

// Test 2: enabled, cap=3, 12 h window → exactly 3 MessageScheduled + 3 MessageSent
#[tokio::test]
async fn enabled_cap3_window12h_yields_exactly_3_scheduled() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 99);
    let proactive = make_proactive(true, 3, Some("22:00"), Some("23:59"));
    let notifier = Arc::new(FakeNotifier::delivered());
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        notifier.clone(),
    );

    let ticks = 12 * 60;
    let mut sent_count = 0usize;
    for _ in 0..ticks {
        let now_utc = clock.now_utc();
        let now_local = clock.now_local();
        let outcome = outbox.run_tick(now_utc, now_local).await;
        if matches!(outcome, TickOutcome::Sent { .. }) {
            sent_count += 1;
        }
        clock.advance(Duration::seconds(60));
    }

    assert_eq!(
        sent_count, 3,
        "expected exactly 3 Sent outcomes across a 12h window with cap=3"
    );
    assert_eq!(
        count_scheduled(tmp.path()),
        3,
        "ledger must have exactly 3 MessageScheduled events"
    );
}

// Test 3: morning_greeting only fires once per day
#[tokio::test]
async fn morning_greeting_only_once_per_day() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 6, 30, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 0);
    let proactive = make_proactive(true, 10, None, None);
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        Arc::new(FakeNotifier::delivered()),
    );

    let mut morning_count = 0usize;
    for _ in 0..48 {
        let now_utc = clock.now_utc();
        let now_local = clock.now_local();
        if let TickOutcome::Sent { situation, .. } = outbox.run_tick(now_utc, now_local).await
            && situation == Situation::MorningGreeting
        {
            morning_count += 1;
        }
        clock.advance(Duration::minutes(5));
    }

    assert_eq!(
        morning_count, 1,
        "MorningGreeting should fire at most once per day; got {morning_count}"
    );
}

// Test 4: day rollover resets counters
#[tokio::test]
async fn day_rollover_resets_counters() {
    let tmp = TempDir::new().unwrap();
    let base_utc = local_as_utc(2026, 4, 29, 10, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(tmp.path()).unwrap();
    let picker = Picker::with_seed(seed_bandit_state(), 7);
    // cap=1 → only 1 send per day
    let proactive = make_proactive(true, 1, Some("22:00"), Some("23:59"));
    let mut outbox = make_outbox(
        clock.clone(),
        ledger,
        picker,
        proactive,
        stub_clean_zh(),
        Arc::new(FakeNotifier::delivered()),
    );

    // Day N: first tick should send
    let outcome_day_n = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert!(
        matches!(outcome_day_n, TickOutcome::Sent { .. }),
        "day N first tick should send; got {outcome_day_n:?}"
    );
    assert_eq!(outbox.sent_today, 1);

    // Advance 25 h → day N+1
    clock.advance(Duration::hours(25));

    let outcome_day_n1 = outbox.run_tick(clock.now_utc(), clock.now_local()).await;
    assert_eq!(
        outbox.sent_today, 1,
        "after rollover sent_today should be 1 (just incremented from 0)"
    );
    assert!(
        matches!(outcome_day_n1, TickOutcome::Sent { .. }),
        "day N+1 first tick should send after rollover; got {outcome_day_n1:?}"
    );

    assert_eq!(
        count_scheduled(tmp.path()),
        2,
        "ledger should have 2 MessageScheduled events (one per day)"
    );
}
